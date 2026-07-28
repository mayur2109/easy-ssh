use crate::model::{Agent, Connection, Project, ProjectFile, TerminalMode};
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
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Position, Rect};
use ratatui::style::Stylize;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, TableState, Wrap,
};
use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
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
            app.poll_background();
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
    query_cursor: usize,
    searching: bool,
    screen: Screen,
    status: Option<(bool, String)>,
    aliases: Vec<String>,
    path_lookup_tx: Sender<PathLookup>,
    path_lookup_rx: Receiver<PathLookup>,
    next_path_request: u64,
    resolved_paths: HashMap<String, String>,
    resolved_path_tx: Sender<RemotePathResolution>,
    resolved_path_rx: Receiver<RemotePathResolution>,
}

enum Screen {
    Browse,
    Form(Box<ProjectForm>),
    ConfirmDelete,
    Settings { original_terminal: TerminalMode },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    agent: Agent,
    error: Option<String>,
    cursor: usize,
    suggestions: Vec<String>,
    suggestion_index: usize,
    path_request: u64,
}

struct PathLookup {
    request: u64,
    result: Result<Vec<String>, String>,
}

struct RemotePathResolution {
    project: String,
    result: Result<String, String>,
}

impl App {
    fn new(paths: AppPaths, projects: ProjectFile) -> Self {
        let aliases = ssh::discover_aliases(&paths);
        let (path_lookup_tx, path_lookup_rx) = mpsc::channel();
        let (resolved_path_tx, resolved_path_rx) = mpsc::channel();
        for project in &projects.projects {
            spawn_remote_path_resolution(project.clone(), resolved_path_tx.clone());
        }
        Self {
            paths,
            projects,
            selected: 0,
            query: String::new(),
            query_cursor: 0,
            searching: false,
            screen: Screen::Browse,
            status: None,
            aliases,
            path_lookup_tx,
            path_lookup_rx,
            next_path_request: 0,
            resolved_paths: HashMap::new(),
            resolved_path_tx,
            resolved_path_rx,
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
        self.poll_background();
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
            Screen::Settings { original_terminal } => {
                self.handle_settings(key, original_terminal)?;
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

    fn poll_background(&mut self) {
        self.receive_path_lookups();
        self.receive_remote_path_resolutions();
    }

    fn handle_browse(&mut self, key: KeyEvent) -> Result<Option<TuiExit>> {
        if self.searching {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.searching = false,
                KeyCode::Backspace => {
                    if self.query_cursor > 0 {
                        let start = self.query[..self.query_cursor]
                            .char_indices()
                            .last()
                            .map(|(index, _)| index)
                            .unwrap_or(0);
                        self.query.drain(start..self.query_cursor);
                        self.query_cursor = start;
                    }
                    self.selected = 0;
                }
                KeyCode::Delete => {
                    let end = self.query[self.query_cursor..]
                        .chars()
                        .next()
                        .map(|character| self.query_cursor + character.len_utf8())
                        .unwrap_or(self.query_cursor);
                    self.query.drain(self.query_cursor..end);
                    self.selected = 0;
                }
                KeyCode::Left => {
                    self.query_cursor = self.query[..self.query_cursor]
                        .char_indices()
                        .last()
                        .map(|(index, _)| index)
                        .unwrap_or(0);
                }
                KeyCode::Right => {
                    self.query_cursor = self.query[self.query_cursor..]
                        .chars()
                        .next()
                        .map(|character| self.query_cursor + character.len_utf8())
                        .unwrap_or(self.query.len());
                }
                KeyCode::Home => self.query_cursor = 0,
                KeyCode::End => self.query_cursor = self.query.len(),
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.insert(self.query_cursor, character);
                    self.query_cursor += character.len_utf8();
                    self.selected = 0;
                }
                _ => {}
            }
            return Ok(None);
        }

        let visible_len = self.visible_indices().len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(Some(TuiExit::Done)),
            KeyCode::Char('/') => {
                self.searching = true;
                self.query_cursor = self.query.len();
            }
            KeyCode::Char('a') => self.screen = Screen::Form(Box::new(ProjectForm::new())),
            KeyCode::Char('e') => {
                if let Some(project) = self.selected_project().cloned() {
                    self.screen = Screen::Form(Box::new(ProjectForm::edit(&project)));
                }
            }
            KeyCode::Char('d') | KeyCode::Delete if self.selected_project().is_some() => {
                self.screen = Screen::ConfirmDelete;
            }
            KeyCode::Char('i') => match ssh::selected_shells(&[])
                .and_then(|shells| ssh::install(&self.paths, &self.projects, &shells))
            {
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
            KeyCode::Char('s') => {
                self.screen = Screen::Settings {
                    original_terminal: self.projects.settings.terminal,
                }
            }
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

    fn handle_settings(&mut self, key: KeyEvent, original_terminal: TerminalMode) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.projects.settings.terminal = original_terminal;
                self.screen = Screen::Browse;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                self.projects.settings.terminal = match self.projects.settings.terminal {
                    TerminalMode::Current => TerminalMode::Ghostty,
                    TerminalMode::Ghostty => TerminalMode::Current,
                };
            }
            KeyCode::Enter => {
                storage::save(&self.paths, &self.projects)?;
                self.status = Some((true, "Terminal preference saved".into()));
                self.screen = Screen::Browse;
            }
            _ => {}
        }
        Ok(())
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
                        self.resolved_paths.remove(&project.name);
                        spawn_remote_path_resolution(
                            project.clone(),
                            self.resolved_path_tx.clone(),
                        );
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
                if form.has_suggestions() && matches!(key.code, KeyCode::Down) {
                    form.select_suggestion(true);
                } else if matches!(key.code, KeyCode::Tab) && form.has_suggestions() {
                    form.accept_suggestion();
                } else {
                    form.focus = (form.focus + 1) % form.field_count();
                    form.cursor_to_end();
                }
                form.error = None;
            }
            KeyCode::BackTab | KeyCode::Up => {
                if form.has_suggestions() && matches!(key.code, KeyCode::Up) {
                    form.select_suggestion(false);
                } else {
                    form.focus = form
                        .focus
                        .checked_sub(1)
                        .unwrap_or_else(|| form.field_count() - 1);
                    form.cursor_to_end();
                }
                form.error = None;
            }
            KeyCode::Left | KeyCode::Right if form.focus == 1 => {
                form.toggle_mode();
                form.cursor_to_end();
                form.error = None;
            }
            KeyCode::Left | KeyCode::Right if form.focus == form.agent_focus() => {
                form.cycle_agent(matches!(key.code, KeyCode::Right));
                form.error = None;
            }
            KeyCode::Left if form.is_editable_focus() => {
                form.move_cursor_left();
            }
            KeyCode::Right if form.is_editable_focus() => {
                form.move_cursor_right();
            }
            KeyCode::Home if form.is_editable_focus() => form.cursor = 0,
            KeyCode::End if form.is_editable_focus() => form.cursor_to_end(),
            KeyCode::Delete if form.is_editable_focus() => {
                form.delete_at_cursor();
                self.request_path_suggestions(form);
            }
            KeyCode::Backspace if form.is_editable_focus() => {
                form.backspace();
                self.request_path_suggestions(form);
                form.error = None;
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && form.is_editable_focus() =>
            {
                form.insert(character);
                self.request_path_suggestions(form);
                form.error = None;
            }
            _ => {}
        }
        Ok(false)
    }

    fn receive_path_lookups(&mut self) {
        while let Ok(lookup) = self.path_lookup_rx.try_recv() {
            let Screen::Form(form) = &mut self.screen else {
                continue;
            };
            if lookup.request != form.path_request {
                continue;
            }
            match lookup.result {
                Ok(suggestions) => {
                    form.suggestions = suggestions;
                    form.suggestion_index = 0;
                }
                Err(error) => {
                    form.suggestions.clear();
                    form.error = Some(error);
                }
            }
        }
    }

    fn receive_remote_path_resolutions(&mut self) {
        while let Ok(resolution) = self.resolved_path_rx.try_recv() {
            if let Ok(path) = resolution.result {
                self.resolved_paths.insert(resolution.project, path);
            }
        }
    }

    fn request_path_suggestions(&mut self, form: &mut ProjectForm) {
        if form.focus != form.remote_path_focus() || form.remote_path.trim().is_empty() {
            form.suggestions.clear();
            return;
        }
        self.next_path_request += 1;
        form.path_request = self.next_path_request;
        let request = form.path_request;
        let path = form.remote_path.clone();
        let connection = form.connection_for_lookup();
        let sender = self.path_lookup_tx.clone();
        thread::spawn(move || {
            let result = lookup_remote_paths(connection, &path);
            let _ = sender.send(PathLookup { request, result });
        });
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
            format!(
                "Search: {}_{}",
                &self.query[..self.query_cursor],
                &self.query[self.query_cursor..]
            )
        } else if let Some((ok, message)) = &self.status {
            format!("{} {message}", if *ok { "OK" } else { "Error:" })
        } else {
            "Enter connect   a add   e edit   d delete   / search   i install   s settings   q quit"
                .into()
        };
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(MUTED)),
            chunks[2],
        );

        match &self.screen {
            Screen::Form(form) => self.draw_form(frame, form),
            Screen::ConfirmDelete => self.draw_delete_confirmation(frame),
            Screen::Settings { .. } => self.draw_settings(frame),
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
                Cell::from(
                    self.resolved_paths
                        .get(&project.name)
                        .cloned()
                        .unwrap_or_else(|| project.remote_path.clone()),
                ),
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
        if frame.area().width < 28 || frame.area().height < 8 {
            frame.render_widget(Clear, frame.area());
            frame.render_widget(
                Paragraph::new("Terminal too small for the project form")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(Color::Yellow)),
                frame.area(),
            );
            return;
        }

        let width = frame.area().width.saturating_sub(4).min(72);
        let height = (form.field_count() as u16 * 3 + 4).min(frame.area().height.saturating_sub(2));
        let area = centered_rect(width, height, frame.area());
        frame.render_widget(Clear, area);
        let action = if form.original_name.is_some() {
            "Edit project"
        } else {
            "Add project"
        };
        let inner_height = area.height.saturating_sub(2);
        let visible_count = usize::from(inner_height.saturating_sub(2) / 3)
            .max(1)
            .min(form.field_count());
        let first_visible = form
            .focus
            .saturating_add(1)
            .saturating_sub(visible_count)
            .min(form.field_count().saturating_sub(visible_count));
        let last_visible = first_visible + visible_count;
        let title = if visible_count < form.field_count() {
            format!(
                " {action} · fields {}-{} of {} ",
                first_visible + 1,
                last_visible,
                form.field_count()
            )
        } else {
            format!(" {action} ")
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut constraints = vec![Constraint::Length(3); visible_count];
        constraints.push(Constraint::Min(2));
        let rows = Layout::vertical(constraints).split(inner);
        let fields = form.fields();
        for (row_index, field_index) in (first_visible..last_visible).enumerate() {
            let (label, value) = &fields[field_index];
            let row = rows[row_index];
            if row.height < 3 {
                continue;
            }
            let focused = field_index == form.focus;
            let border = if focused { Color::Yellow } else { MUTED };
            let field_title = if focused {
                format!(" > {label} ")
            } else {
                format!(" {label} ")
            };
            let input_width = row.width.saturating_sub(2);
            let (scroll, cursor_column) =
                input_view(value, form.cursor_for(field_index), input_width);
            let value_style = if focused {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let widget = Paragraph::new(value.as_str())
                .style(value_style)
                .scroll((0, scroll))
                .block(
                    Block::default()
                        .title(field_title)
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(border)),
                );
            frame.render_widget(widget, row);

            if focused && form.is_editable_focus() && input_width > 0 {
                frame.set_cursor_position(Position::new(row.x + 1 + cursor_column, row.y + 1));
            }
        }
        if form.focus == form.remote_path_focus() && form.has_suggestions() {
            let suggestion_height = (form.suggestions.len().min(6) as u16) + 2;
            let suggestion_area = Rect {
                x: area.x.saturating_add(2),
                y: area.y.saturating_add(area.height.saturating_sub(2)),
                width: area.width.saturating_sub(4),
                height: suggestion_height.min(
                    frame
                        .area()
                        .bottom()
                        .saturating_sub(area.y + area.height.saturating_sub(2)),
                ),
            };
            let items = form
                .suggestions
                .iter()
                .enumerate()
                .take(6)
                .map(|(index, path)| {
                    let prefix = if index == form.suggestion_index {
                        "› "
                    } else {
                        "  "
                    };
                    ListItem::new(format!("{prefix}{path}"))
                });
            frame.render_widget(
                List::new(items).block(
                    Block::default()
                        .title(" Remote matches ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Yellow)),
                ),
                suggestion_area,
            );
        }
        let focused_label = fields[form.focus].0;
        let help = form
            .error
            .clone()
            .unwrap_or_else(|| {
                format!(
                    "Field: {focused_label} | Tab/Up/Down move | Left/Right choose | Enter save | Esc cancel"
                )
            });
        let style = if form.error.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(MUTED)
        };
        frame.render_widget(
            Paragraph::new(help).style(style).wrap(Wrap { trim: true }),
            rows[visible_count],
        );
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

    fn draw_settings(&self, frame: &mut ratatui::Frame) {
        let area = centered_rect(58.min(frame.area().width), 7, frame.area());
        frame.render_widget(Clear, area);
        let terminal = match self.projects.settings.terminal {
            TerminalMode::Current => "Current terminal",
            TerminalMode::Ghostty => "Ghostty",
        };
        frame.render_widget(
            Paragraph::new(format!(
                "Default terminal: {terminal}\n\nLeft/Right/Space change   Enter save   Esc cancel"
            ))
            .block(
                Block::default()
                    .title(" Settings ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT)),
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
            agent: Agent::Shell,
            error: None,
            cursor: 0,
            suggestions: Vec::new(),
            suggestion_index: 0,
            path_request: 0,
        }
    }

    fn edit(project: &Project) -> Self {
        let mut form = Self::new();
        form.original_name = Some(project.name.clone());
        form.name = project.name.clone();
        form.remote_path = project.remote_path.clone();
        form.agent = project.agent;
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
        form.cursor_to_end();
        form
    }

    fn field_count(&self) -> usize {
        if self.mode == ConnectionMode::New {
            8
        } else {
            5
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
                ("Agent", self.agent.label().into()),
            ]
        } else {
            vec![
                ("Project name", self.name.clone()),
                ("Connection", mode.into()),
                ("SSH alias", self.target.clone()),
                ("Remote folder", self.remote_path.clone()),
                ("Agent", self.agent.label().into()),
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

    fn current_value(&self) -> &str {
        if self.mode == ConnectionMode::New {
            match self.focus {
                0 => &self.name,
                2 => &self.target,
                3 => &self.user,
                4 => &self.port,
                5 => &self.identity,
                6 => &self.remote_path,
                7 => "",
                _ => "",
            }
        } else {
            match self.focus {
                0 => &self.name,
                2 => &self.target,
                3 => &self.remote_path,
                4 => "",
                _ => "",
            }
        }
    }

    fn cursor_for(&self, field_index: usize) -> usize {
        if field_index == self.focus && self.is_editable_focus() {
            self.cursor
        } else {
            0
        }
    }

    fn cursor_to_end(&mut self) {
        self.cursor = self.current_value().len();
    }

    fn move_cursor_left(&mut self) {
        self.cursor = self.current_value()[..self.cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0);
    }

    fn move_cursor_right(&mut self) {
        let value = self.current_value();
        self.cursor = value[self.cursor..]
            .chars()
            .next()
            .map(|character| self.cursor + character.len_utf8())
            .unwrap_or(value.len());
    }

    fn insert(&mut self, character: char) {
        let cursor = self.cursor;
        self.current_value_mut().insert(cursor, character);
        self.cursor += character.len_utf8();
        self.suggestions.clear();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.move_cursor_left();
        let cursor = self.cursor;
        let end = self.current_value()[cursor..]
            .chars()
            .next()
            .map(|character| cursor + character.len_utf8())
            .unwrap_or(cursor);
        self.current_value_mut().drain(cursor..end);
        self.suggestions.clear();
    }

    fn delete_at_cursor(&mut self) {
        let cursor = self.cursor;
        let end = self.current_value()[cursor..]
            .chars()
            .next()
            .map(|character| cursor + character.len_utf8())
            .unwrap_or(cursor);
        self.current_value_mut().drain(cursor..end);
        self.suggestions.clear();
    }

    fn remote_path_focus(&self) -> usize {
        if self.mode == ConnectionMode::New {
            6
        } else {
            3
        }
    }

    fn agent_focus(&self) -> usize {
        if self.mode == ConnectionMode::New {
            7
        } else {
            4
        }
    }

    fn cycle_agent(&mut self, forward: bool) {
        self.agent = match (self.agent, forward) {
            (Agent::Shell, true) | (Agent::Codex, false) => Agent::Claude,
            (Agent::Claude, true) | (Agent::Shell, false) => Agent::Codex,
            (Agent::Codex, true) | (Agent::Claude, false) => Agent::Shell,
        };
    }

    fn has_suggestions(&self) -> bool {
        !self.suggestions.is_empty()
    }

    fn select_suggestion(&mut self, forward: bool) {
        if self.suggestions.is_empty() {
            return;
        }
        self.suggestion_index = if forward {
            (self.suggestion_index + 1) % self.suggestions.len()
        } else {
            self.suggestion_index
                .checked_sub(1)
                .unwrap_or(self.suggestions.len() - 1)
        };
    }

    fn accept_suggestion(&mut self) {
        if let Some(path) = self.suggestions.get(self.suggestion_index).cloned() {
            self.remote_path = path;
            self.focus = self.remote_path_focus();
            self.cursor_to_end();
        }
        self.suggestions.clear();
    }

    fn connection_for_lookup(&self) -> LookupConnection {
        match self.mode {
            ConnectionMode::Existing => LookupConnection::Alias(self.target.trim().to_owned()),
            ConnectionMode::New => LookupConnection::NewHost {
                host: self.target.trim().to_owned(),
                user: optional_string(&self.user),
                port: self.port.trim().parse().ok(),
                identity: optional_string(&self.identity),
            },
        }
    }

    fn is_editable_focus(&self) -> bool {
        self.focus != 1 && self.focus != self.agent_focus()
    }

    fn toggle_mode(&mut self) {
        self.focus = match (self.mode, self.focus) {
            (ConnectionMode::New, 6) => 3,
            (ConnectionMode::New, 7) => 4,
            (ConnectionMode::New, 3..=5) => 2,
            (ConnectionMode::Existing, 3) => 6,
            (ConnectionMode::Existing, 4) => 7,
            (_, focus) => focus,
        };
        self.mode = if self.mode == ConnectionMode::New {
            ConnectionMode::Existing
        } else {
            ConnectionMode::New
        };
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
            agent: self.agent,
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

fn input_view(value: &str, cursor: usize, width: u16) -> (u16, u16) {
    if width == 0 {
        return (0, 0);
    }
    let cursor_width = Line::from(&value[..cursor]).width();
    let cursor_limit = usize::from(width.saturating_sub(1));
    let scroll = cursor_width.saturating_sub(cursor_limit);
    (
        scroll.min(usize::from(u16::MAX)) as u16,
        cursor_width.saturating_sub(scroll).min(cursor_limit) as u16,
    )
}

#[derive(Clone)]
enum LookupConnection {
    Alias(String),
    NewHost {
        host: String,
        user: Option<String>,
        port: Option<u16>,
        identity: Option<String>,
    },
}

fn spawn_remote_path_resolution(project: Project, sender: Sender<RemotePathResolution>) {
    thread::spawn(move || {
        let result = resolve_remote_path(
            lookup_connection_for_project(&project),
            &project.remote_path,
        );
        let _ = sender.send(RemotePathResolution {
            project: project.name,
            result,
        });
    });
}

fn lookup_connection_for_project(project: &Project) -> LookupConnection {
    match &project.connection {
        Connection::ExistingAlias { ssh_alias } => LookupConnection::Alias(ssh_alias.clone()),
        Connection::NewHost {
            host,
            user,
            port,
            identity_file,
        } => LookupConnection::NewHost {
            host: host.clone(),
            user: user.clone(),
            port: *port,
            identity: identity_file
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
        },
    }
}

fn command_for_lookup(connection: LookupConnection) -> Result<Command, String> {
    let mut command = Command::new("ssh");
    match connection {
        LookupConnection::Alias(alias) => {
            if alias.is_empty() {
                return Err("no SSH alias configured".into());
            }
            command.arg("--").arg(alias);
        }
        LookupConnection::NewHost {
            host,
            user,
            port,
            identity,
        } => {
            if host.is_empty() {
                return Err("no host configured".into());
            }
            if let Some(port) = port {
                command.args(["-p", &port.to_string()]);
            }
            if let Some(identity) = identity {
                command.args(["-i", &identity]);
            }
            let target = user.map(|user| format!("{user}@{host}")).unwrap_or(host);
            command.arg("--").arg(target);
        }
    }
    Ok(command)
}

fn resolve_remote_path(connection: LookupConnection, remote_path: &str) -> Result<String, String> {
    let remote = format!(
        "p={}; case \"$p\" in '~') p=\"$HOME\";; '~/'*) p=\"$HOME/${{p#~/}}\";; /*) ;; *) p=\"$HOME/$p\";; esac; if cd -- \"$p\" 2>/dev/null; then pwd -P; else printf '%s\\n' \"$p\"; fi",
        shell_quote_for_remote(remote_path)
    );
    let output = command_for_lookup(connection)?
        .arg(remote)
        .output()
        .map_err(|error| format!("path resolution failed: {error}"))?;
    if !output.status.success() {
        return Err("path resolution failed on remote host".into());
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .last()
        .filter(|path| path.starts_with('/'))
        .map(str::to_owned)
        .ok_or_else(|| "remote host did not return an absolute path".into())
}

fn lookup_remote_paths(
    connection: LookupConnection,
    typed_path: &str,
) -> Result<Vec<String>, String> {
    let (parent, fragment) = split_path_query(typed_path);
    let Ok(mut command) = command_for_lookup(connection) else {
        return Ok(Vec::new());
    };
    let remote = format!(
        "p={}; case \"$p\" in '~') p=\"$HOME\";; '~/'*) p=\"$HOME/${{p#~/}}\";; esac; if [ -d \"$p\" ]; then find \"$p\" -mindepth 1 -maxdepth 1 -type d -printf '%f\\n' 2>/dev/null; fi",
        shell_quote_for_remote(&parent)
    );
    let output = command
        .arg(remote)
        .output()
        .map_err(|error| format!("path lookup failed: {error}"))?;
    if !output.status.success() {
        return Err("path lookup failed on remote host".into());
    }
    let mut results = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|name| format_path_candidate(&parent, name))
        .filter(|candidate| fuzzy_matches(candidate, typed_path, &fragment))
        .collect::<Vec<_>>();
    results.sort_by_key(|candidate| fuzzy_score(candidate, typed_path));
    Ok(results)
}

fn split_path_query(value: &str) -> (String, String) {
    let trimmed = value.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some(("", fragment)) => ("/".into(), fragment.into()),
        Some((parent, fragment)) => (parent.into(), fragment.into()),
        None => (".".into(), trimmed.into()),
    }
}

fn format_path_candidate(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else if parent == "." {
        name.into()
    } else {
        format!("{parent}/{name}")
    }
}

fn fuzzy_matches(candidate: &str, typed: &str, fragment: &str) -> bool {
    let needle = if fragment.is_empty() { typed } else { fragment }.to_ascii_lowercase();
    let mut chars = needle.chars();
    let mut next = chars.next();
    for character in candidate.to_ascii_lowercase().chars() {
        if Some(character) == next {
            next = chars.next();
        }
    }
    next.is_none()
}

fn fuzzy_score(candidate: &str, typed: &str) -> (usize, String) {
    (
        candidate.len().saturating_sub(typed.len()),
        candidate.to_ascii_lowercase(),
    )
}

fn shell_quote_for_remote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\\"'\\\"'"))
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
    use ratatui::backend::TestBackend;

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

    #[test]
    fn connection_selector_is_not_editable() {
        let paths = AppPaths::for_home_and_config(
            PathBuf::from("/tmp/easy-ssh-tui-test"),
            PathBuf::from("/tmp/easy-ssh-tui-test/config"),
        );
        let mut app = App::new(paths, ProjectFile::default());
        let mut form = ProjectForm::new();
        form.target = "server.example.com".into();
        form.focus = 1;

        app.handle_form(
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &mut form,
        )
        .unwrap();

        assert_eq!(form.target, "server.example.com");
    }

    #[test]
    fn mode_toggle_preserves_remote_path_focus() {
        let mut form = ProjectForm::new();
        form.focus = 6;
        form.toggle_mode();
        assert_eq!(form.mode, ConnectionMode::Existing);
        assert_eq!(form.focus, 3);

        form.toggle_mode();
        assert_eq!(form.mode, ConnectionMode::New);
        assert_eq!(form.focus, 6);
    }

    #[test]
    fn long_input_keeps_cursor_inside_field() {
        assert_eq!(input_view("short", 5, 10), (0, 5));
        assert_eq!(input_view("a very long remote path", 23, 10), (14, 9));
    }

    #[test]
    fn editable_fields_insert_and_delete_at_the_cursor() {
        let mut form = ProjectForm::new();
        form.name = "ab".into();
        form.cursor = 1;
        form.insert('é');
        assert_eq!(form.name, "aéb");
        form.backspace();
        assert_eq!(form.name, "ab");
        form.delete_at_cursor();
        assert_eq!(form.name, "a");
    }

    #[test]
    fn fuzzy_path_matching_accepts_non_contiguous_letters() {
        assert!(fuzzy_matches("~/work/avarya-api", "~/work/aai", "aai"));
        assert!(!fuzzy_matches("~/work/api", "~/work/xyz", "xyz"));
    }

    #[test]
    fn form_draws_focus_marker_and_terminal_cursor() {
        let paths = AppPaths::for_home_and_config(
            PathBuf::from("/tmp/easy-ssh-tui-render"),
            PathBuf::from("/tmp/easy-ssh-tui-render/config"),
        );
        let mut app = App::new(paths, ProjectFile::default());
        let mut form = ProjectForm::new();
        form.name = "avarya".into();
        app.screen = Screen::Form(Box::new(form));
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| app.draw(frame)).unwrap();

        let cursor = terminal.get_cursor_position().unwrap();
        assert!(cursor.x < 80 && cursor.y < 24);
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("> Project name"));
    }

    #[test]
    fn compact_form_keeps_the_focused_field_visible() {
        let paths = AppPaths::for_home_and_config(
            PathBuf::from("/tmp/easy-ssh-tui-compact"),
            PathBuf::from("/tmp/easy-ssh-tui-compact/config"),
        );
        let mut app = App::new(paths, ProjectFile::default());
        let mut form = ProjectForm::new();
        form.focus = 6;
        form.remote_path = "~/projects/avarya".into();
        app.screen = Screen::Form(Box::new(form));
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| app.draw(frame)).unwrap();

        let cursor = terminal.get_cursor_position().unwrap();
        assert!(cursor.x < 40 && cursor.y < 10);
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("fields 7-7 of 8"));
        assert!(rendered.contains("> Remote folder"));
    }
}
