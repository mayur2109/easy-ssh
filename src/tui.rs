use crate::model::{Connection, Project, ProjectFile};
use crate::ssh;
use crate::storage::{self, AppPaths};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, TableState, Wrap,
};
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;

pub enum TuiExit {
    Done,
    Connect(String),
}

pub fn run(paths: &AppPaths, projects: ProjectFile) -> Result<TuiExit> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        anyhow::bail!(
            "the TUI requires an interactive terminal; use `easy-ssh --help` for CLI commands"
        );
    }
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => {
            disable_raw_mode()?;
            let mut stdout = io::stdout();
            execute!(stdout, LeaveAlternateScreen)?;
            return Err(error.into());
        }
    };
    let mut app = App::new(paths.clone(), projects);

    let result = (|| -> Result<TuiExit> {
        loop {
            terminal.draw(|frame| app.draw(frame))?;
            if event::poll(Duration::from_millis(200))?
                && let Event::Key(key) = event::read()?
                && let Some(exit) = app.handle_key(key)?
            {
                return Ok(exit);
            }
        }
    })();

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

struct App {
    paths: AppPaths,
    projects: ProjectFile,
    selected: usize,
    query: String,
    searching: bool,
    screen: Screen,
    status: Option<(bool, String)>,
    aliases: Vec<String>,
}

enum Screen {
    Browse,
    Form(Box<ProjectForm>),
    ConfirmDelete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConnectionMode {
    New,
    Existing,
}

struct ProjectForm {
    original_name: Option<String>,
    mode: ConnectionMode,
    focus: usize,
    name: String,
    target: String,
    user: String,
    port: String,
    identity: String,
    remote_path: String,
    error: Option<String>,
    alias_index: usize,
}

impl App {
    fn new(paths: AppPaths, projects: ProjectFile) -> Self {
        let aliases = ssh::discover_aliases(&paths);
        Self {
            paths,
            projects,
            selected: 0,
            query: String::new(),
            searching: false,
            screen: Screen::Browse,
            status: None,
            aliases,
        }
    }

    fn visible_indices(&self) -> Vec<usize> {
        let query = self.query.to_ascii_lowercase();
        self.projects
            .projects
            .iter()
            .enumerate()
            .filter(|(_, project)| {
                query.is_empty()
                    || project.name.to_ascii_lowercase().contains(&query)
                    || project.destination().to_ascii_lowercase().contains(&query)
                    || project.remote_path.to_ascii_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_project(&self) -> Option<&Project> {
        self.visible_indices()
            .get(self.selected)
            .and_then(|index| self.projects.projects.get(*index))
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<Option<TuiExit>> {
        let screen = std::mem::replace(&mut self.screen, Screen::Browse);
        match screen {
            Screen::Browse => {
                self.screen = Screen::Browse;
                self.handle_browse(key)
            }
            Screen::ConfirmDelete => {
                self.handle_delete_confirmation(key)?;
                Ok(None)
            }
            Screen::Form(mut form) => {
                if self.handle_form(key, &mut form)? {
                    self.screen = Screen::Browse;
                } else {
                    self.screen = Screen::Form(form);
                }
                Ok(None)
            }
        }
    }

    fn handle_browse(&mut self, key: KeyEvent) -> Result<Option<TuiExit>> {
        if self.searching {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    self.query.pop();
                    self.selected = 0;
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.push(character);
                    self.selected = 0;
                }
                _ => {}
            }
            return Ok(None);
        }

        let visible_len = self.visible_indices().len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(Some(TuiExit::Done)),
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Char('a') => self.screen = Screen::Form(Box::new(ProjectForm::new())),
            KeyCode::Char('e') => {
                if let Some(project) = self.selected_project().cloned() {
                    self.screen = Screen::Form(Box::new(ProjectForm::edit(&project)));
                }
            }
            KeyCode::Char('d') | KeyCode::Delete if self.selected_project().is_some() => {
                self.screen = Screen::ConfirmDelete;
            }
            KeyCode::Char('i') => match ssh::install(&self.paths, &self.projects) {
                Ok(changes) => {
                    self.status = Some((
                        true,
                        if changes.is_empty() {
                            "Shell integration is already installed".into()
                        } else {
                            "Shell integration installed; reload your shell".into()
                        },
                    ))
                }
                Err(error) => self.status = Some((false, error.to_string())),
            },
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.selected + 1 < visible_len => {
                self.selected += 1;
            }
            KeyCode::Enter => {
                if let Some(project) = self.selected_project() {
                    return Ok(Some(TuiExit::Connect(project.name.clone())));
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn handle_delete_confirmation(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(project) = self.selected_project() {
                    let name = project.name.clone();
                    storage::remove(&mut self.projects, &name)?;
                    storage::save(&self.paths, &self.projects)?;
                    ssh::sync_generated_config(&self.paths, &self.projects)?;
                    self.selected = self
                        .selected
                        .min(self.visible_indices().len().saturating_sub(1));
                    self.status = Some((true, format!("Removed {name}")));
                }
                self.screen = Screen::Browse;
            }
            KeyCode::Char('n') | KeyCode::Esc => self.screen = Screen::Browse,
            _ => self.screen = Screen::ConfirmDelete,
        }
        Ok(())
    }

    fn handle_form(&mut self, key: KeyEvent, form: &mut ProjectForm) -> Result<bool> {
        match key.code {
            KeyCode::Esc => return Ok(true),
            KeyCode::Enter => match form.to_project() {
                Ok(project) => {
                    let mut next = self.projects.clone();
                    if let Err(error) = ssh::validate_connection_name(&self.paths, &project)
                        .and_then(|_| {
                            storage::upsert(
                                &mut next,
                                form.original_name.as_deref(),
                                project.clone(),
                            )
                        })
                    {
                        form.error = Some(error.to_string());
                    } else if let Err(error) = storage::save(&self.paths, &next)
                        .and_then(|_| ssh::sync_generated_config(&self.paths, &next))
                    {
                        form.error = Some(error.to_string());
                    } else {
                        self.projects = next;
                        self.status = Some((true, format!("Saved {}", project.name)));
                        self.aliases = ssh::discover_aliases(&self.paths);
                        self.selected = self
                            .visible_indices()
                            .iter()
                            .position(|index| self.projects.projects[*index].name == project.name)
                            .unwrap_or(0);
                        return Ok(true);
                    }
                }
                Err(error) => form.error = Some(error.to_string()),
            },
            KeyCode::Tab | KeyCode::Down => {
                form.focus = (form.focus + 1) % form.field_count();
                form.error = None;
            }
            KeyCode::BackTab | KeyCode::Up => {
                form.focus = form
                    .focus
                    .checked_sub(1)
                    .unwrap_or_else(|| form.field_count() - 1);
                form.error = None;
            }
            KeyCode::Left | KeyCode::Right if form.focus == 1 => {
                form.mode = if form.mode == ConnectionMode::New {
                    ConnectionMode::Existing
                } else {
                    ConnectionMode::New
                };
                form.focus = form.focus.min(form.field_count() - 1);
                form.error = None;
            }
            KeyCode::Left if form.mode == ConnectionMode::Existing && form.focus == 2 => {
                form.cycle_alias(&self.aliases, false);
            }
            KeyCode::Right if form.mode == ConnectionMode::Existing && form.focus == 2 => {
                form.cycle_alias(&self.aliases, true);
            }
            KeyCode::Backspace => {
                form.current_value_mut().pop();
                form.error = None;
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && form.focus != 1 =>
            {
                form.current_value_mut().push(character);
                form.error = None;
            }
            _ => {}
        }
        Ok(false)
    }

    fn draw(&self, frame: &mut ratatui::Frame) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(2),
            ])
            .split(area);

        let title = Paragraph::new(Line::from(vec![
            Span::styled("easy-ssh", Style::default().fg(ACCENT).bold()),
            Span::raw("  Projects"),
            if self.query.is_empty() {
                Span::raw("")
            } else {
                Span::styled(
                    format!("  filter: {}", self.query),
                    Style::default().fg(Color::Yellow),
                )
            },
        ]))
        .block(Block::default().borders(Borders::BOTTOM));
        frame.render_widget(title, chunks[0]);

        self.draw_table(frame, chunks[1]);
        let footer = if self.searching {
            format!("Search: {}_", self.query)
        } else if let Some((ok, message)) = &self.status {
            format!("{} {message}", if *ok { "OK" } else { "Error:" })
        } else {
            "Enter connect   a add   e edit   d delete   / search   i install   q quit".into()
        };
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(MUTED)),
            chunks[2],
        );

        match &self.screen {
            Screen::Form(form) => self.draw_form(frame, form),
            Screen::ConfirmDelete => self.draw_delete_confirmation(frame),
            Screen::Browse => {}
        }
    }

    fn draw_table(&self, frame: &mut ratatui::Frame, area: Rect) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            let message = if self.projects.projects.is_empty() {
                "No projects yet. Press a to add one."
            } else {
                "No projects match this search."
            };
            frame.render_widget(
                Paragraph::new(message)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(MUTED))
                    .block(Block::default().borders(Borders::BOTTOM)),
                area,
            );
            return;
        }
        let rows = visible.iter().map(|index| {
            let project = &self.projects.projects[*index];
            Row::new(vec![
                Cell::from(project.name.clone()),
                Cell::from(project.destination()),
                Cell::from(project.remote_path.clone()),
            ])
        });
        let header = Row::new(["PROJECT", "DESTINATION", "REMOTE FOLDER"])
            .style(Style::default().fg(MUTED).add_modifier(Modifier::BOLD))
            .bottom_margin(1);
        let table = Table::new(
            rows,
            [
                Constraint::Percentage(22),
                Constraint::Percentage(36),
                Constraint::Percentage(42),
            ],
        )
        .header(header)
        .row_highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .highlight_symbol("› ")
        .block(Block::default().borders(Borders::BOTTOM));
        let mut state = TableState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(table, area, &mut state);
    }

    fn draw_form(&self, frame: &mut ratatui::Frame, form: &ProjectForm) {
        let width = frame.area().width.saturating_sub(4).min(72);
        let height = (form.field_count() as u16 * 3 + 6).min(frame.area().height.saturating_sub(2));
        let area = centered_rect(width, height, frame.area());
        frame.render_widget(Clear, area);
        let title = if form.original_name.is_some() {
            " Edit project "
        } else {
            " Add project "
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut constraints = vec![Constraint::Length(3); form.field_count()];
        constraints.push(Constraint::Min(2));
        let rows = Layout::vertical(constraints).split(inner);
        for (index, (label, value)) in form.fields().into_iter().enumerate() {
            let focused = index == form.focus;
            let border = if focused { ACCENT } else { MUTED };
            let widget = Paragraph::new(value)
                .block(
                    Block::default()
                        .title(format!(" {label} "))
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(border)),
                )
                .wrap(Wrap { trim: false });
            frame.render_widget(widget, rows[index]);
        }
        let help = form
            .error
            .as_deref()
            .unwrap_or("Up/Down fields   Left/Right connection or alias   Enter save   Esc cancel");
        let style = if form.error.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(MUTED)
        };
        frame.render_widget(Paragraph::new(help).style(style), rows[form.field_count()]);
    }

    fn draw_delete_confirmation(&self, frame: &mut ratatui::Frame) {
        let Some(project) = self.selected_project() else {
            return;
        };
        let area = centered_rect(54.min(frame.area().width), 7, frame.area());
        frame.render_widget(Clear, area);
        let message = vec![
            ListItem::new(format!("Remove project \"{}\"?", project.name)),
            ListItem::new(""),
            ListItem::new("Enter/y remove    Esc/n cancel"),
        ];
        frame.render_widget(
            List::new(message).block(
                Block::default()
                    .title(" Confirm ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            ),
            area,
        );
    }
}

impl ProjectForm {
    fn new() -> Self {
        Self {
            original_name: None,
            mode: ConnectionMode::New,
            focus: 0,
            name: String::new(),
            target: String::new(),
            user: String::new(),
            port: String::new(),
            identity: String::new(),
            remote_path: String::new(),
            error: None,
            alias_index: 0,
        }
    }

    fn edit(project: &Project) -> Self {
        let mut form = Self::new();
        form.original_name = Some(project.name.clone());
        form.name = project.name.clone();
        form.remote_path = project.remote_path.clone();
        match &project.connection {
            Connection::NewHost {
                host,
                user,
                port,
                identity_file,
            } => {
                form.target = host.clone();
                form.user = user.clone().unwrap_or_default();
                form.port = port.map(|value| value.to_string()).unwrap_or_default();
                form.identity = identity_file
                    .as_ref()
                    .map(|value| value.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
            Connection::ExistingAlias { ssh_alias } => {
                form.mode = ConnectionMode::Existing;
                form.target = ssh_alias.clone();
            }
        }
        form
    }

    fn field_count(&self) -> usize {
        if self.mode == ConnectionMode::New {
            7
        } else {
            4
        }
    }

    fn fields(&self) -> Vec<(&'static str, String)> {
        let mode = if self.mode == ConnectionMode::New {
            "New host  (Left/Right to change)"
        } else {
            "Existing SSH alias  (Left/Right to change)"
        };
        if self.mode == ConnectionMode::New {
            vec![
                ("Project name", self.name.clone()),
                ("Connection", mode.into()),
                ("Host", self.target.clone()),
                ("User (optional)", self.user.clone()),
                ("Port (optional)", self.port.clone()),
                ("Identity file (optional)", self.identity.clone()),
                ("Remote folder", self.remote_path.clone()),
            ]
        } else {
            vec![
                ("Project name", self.name.clone()),
                ("Connection", mode.into()),
                ("SSH alias", self.target.clone()),
                ("Remote folder", self.remote_path.clone()),
            ]
        }
    }

    fn current_value_mut(&mut self) -> &mut String {
        if self.mode == ConnectionMode::New {
            match self.focus {
                0 => &mut self.name,
                2 => &mut self.target,
                3 => &mut self.user,
                4 => &mut self.port,
                5 => &mut self.identity,
                6 => &mut self.remote_path,
                _ => &mut self.target,
            }
        } else {
            match self.focus {
                0 => &mut self.name,
                2 => &mut self.target,
                3 => &mut self.remote_path,
                _ => &mut self.target,
            }
        }
    }

    fn cycle_alias(&mut self, aliases: &[String], forward: bool) {
        if aliases.is_empty() {
            return;
        }
        if let Some(index) = aliases.iter().position(|alias| alias == &self.target) {
            self.alias_index = index;
        }
        self.alias_index = if forward {
            (self.alias_index + 1) % aliases.len()
        } else {
            self.alias_index.checked_sub(1).unwrap_or(aliases.len() - 1)
        };
        self.target.clone_from(&aliases[self.alias_index]);
    }

    fn to_project(&self) -> Result<Project> {
        let connection = if self.mode == ConnectionMode::New {
            Connection::NewHost {
                host: self.target.trim().to_owned(),
                user: optional_string(&self.user),
                port: if self.port.trim().is_empty() {
                    None
                } else {
                    Some(self.port.trim().parse()?)
                },
                identity_file: optional_string(&self.identity).map(PathBuf::from),
            }
        } else {
            Connection::ExistingAlias {
                ssh_alias: self.target.trim().to_owned(),
            }
        };
        let project = Project {
            name: self.name.trim().to_owned(),
            remote_path: self.remote_path.trim().to_owned(),
            connection,
        };
        project.validate()?;
        Ok(project)
    }
}

fn optional_string(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_builds_existing_alias_project() {
        let mut form = ProjectForm::new();
        form.mode = ConnectionMode::Existing;
        form.name = "avarya".into();
        form.target = "production".into();
        form.remote_path = "~/avarya".into();
        let project = form.to_project().unwrap();
        assert_eq!(project.ssh_target(), "production");
    }
}
