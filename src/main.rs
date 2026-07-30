mod model;
mod ssh;
mod storage;
mod tui;

use crate::model::{Agent, Connection, Project, TerminalMode};
use crate::storage::AppPaths;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::io::{self, Write};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install SSH include and shell integration
    Install {
        /// Shell startup file to configure; repeat to configure both
        #[arg(long, value_enum)]
        shell: Vec<ssh::Shell>,
    },
    /// List saved projects
    List,
    /// Add a project
    Add(AddArgs),
    /// Edit a project
    Edit(EditArgs),
    /// Remove a project
    Remove {
        name: String,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Connect to a project
    Connect {
        name: String,
        /// Terminal to use for this launch
        #[arg(long, value_enum)]
        terminal: Option<TerminalMode>,
        /// Agent to launch or resume for this connection
        #[arg(long, value_enum)]
        agent: Option<Agent>,
    },
    /// Manage persistent Claude and Codex tmux sessions
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Check configuration and shell integration
    Doctor {
        /// Shell startup file to check; repeat to check both
        #[arg(long, value_enum)]
        shell: Vec<ssh::Shell>,
    },
    #[command(name = "__is-project", hide = true)]
    IsProject { name: String },
    #[command(name = "__complete-projects", hide = true)]
    CompleteProjects,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// List saved sessions for a project
    List { name: String },
    /// Create and open a named persistent agent session
    New {
        name: String,
        label: String,
        #[arg(long, value_enum, default_value_t = Agent::Claude)]
        agent: Agent,
    },
    /// Open one saved session, or every active window in its workspace
    Open {
        name: String,
        session: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long, value_enum)]
        terminal: Option<TerminalMode>,
    },
}

#[derive(Debug, Args)]
struct AddArgs {
    name: String,
    /// Hostname for a new SSH connection
    #[arg(long, conflicts_with = "ssh_alias")]
    host: Option<String>,
    /// Existing Host alias from ~/.ssh/config
    #[arg(long, conflicts_with = "host")]
    ssh_alias: Option<String>,
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    identity: Option<PathBuf>,
    /// Directory to enter on the remote machine
    #[arg(long)]
    path: String,
}

#[derive(Debug, Args)]
struct EditArgs {
    name: String,
    /// Rename the project
    #[arg(long)]
    new_name: Option<String>,
    /// Switch to or update a new SSH connection
    #[arg(long, conflicts_with = "ssh_alias")]
    host: Option<String>,
    /// Switch to or update an existing Host alias
    #[arg(long, conflicts_with = "host")]
    ssh_alias: Option<String>,
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    identity: Option<PathBuf>,
    #[arg(long)]
    path: Option<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = AppPaths::discover()?;
    let mut file = storage::load(&paths)?;

    match cli.command {
        None => match tui::run(&paths, file)? {
            tui::TuiExit::Done => Ok(()),
            tui::TuiExit::Connect(name) => {
                let file = storage::load(&paths)?;
                let project = find_project(&file, &name)?;
                ssh::connect_with_terminal(project, file.settings.terminal, Agent::Claude)
            }
            tui::TuiExit::OpenSession { project: name, id } => {
                let file = storage::load(&paths)?;
                let project = find_project(&file, &name)?;
                let registry = storage::load_sessions(&paths)?;
                let session = registry
                    .sessions
                    .iter()
                    .find(|item| item.project == name && item.id == id)
                    .with_context(|| format!("session {id:?} does not exist"))?;
                open_saved_session(project, session, file.settings.terminal)
            }
            tui::TuiExit::OpenWorkspace(name) => {
                let file = storage::load(&paths)?;
                ssh::connect_workspace(find_project(&file, &name)?, file.settings.terminal)
            }
            tui::TuiExit::NewSession {
                project: name,
                label,
                agent,
            } => {
                let file = storage::load(&paths)?;
                let project = find_project(&file, &name)?;
                let session =
                    ssh::create_agent_session(project, agent, &Uuid::new_v4().to_string(), &label)?;
                storage::record_session(&paths, session.clone())?;
                ssh::connect_session(project, &session, file.settings.terminal)
            }
        },
        Some(Command::Install { shell }) => {
            let shells = ssh::selected_shells(&shell)?;
            if !paths.registry.exists() {
                storage::save(&paths, &file)?;
            }
            for change in ssh::install(&paths, &file, &shells)? {
                println!("{change}");
            }
            for shell in shells {
                match shell {
                    ssh::Shell::Bash => println!("Reload Bash with: source ~/.bashrc"),
                    ssh::Shell::Zsh => println!("Reload Zsh with: source ~/.zshrc"),
                }
            }
            Ok(())
        }
        Some(Command::List) => {
            if file.projects.is_empty() {
                println!("No projects configured.");
                return Ok(());
            }
            println!("{:<20} {:<32} REMOTE FOLDER", "PROJECT", "DESTINATION");
            for project in &file.projects {
                println!(
                    "{:<20} {:<32} {}",
                    project.name,
                    project.destination(),
                    project.remote_path
                );
            }
            Ok(())
        }
        Some(Command::Add(args)) => {
            let project = project_from_add(args)?;
            let name = project.name.clone();
            ssh::validate_connection_name(&paths, &project)?;
            storage::upsert(&mut file, None, project)?;
            persist(&paths, &file)?;
            println!("Added {name}");
            Ok(())
        }
        Some(Command::Edit(args)) => {
            let project = project_from_edit(&file, &args)?;
            let old_name = args.name.clone();
            let new_name = project.name.clone();
            ssh::validate_connection_name(&paths, &project)?;
            storage::upsert(&mut file, Some(&old_name), project)?;
            persist(&paths, &file)?;
            println!("Updated {old_name} as {new_name}");
            Ok(())
        }
        Some(Command::Remove { name, yes }) => {
            if !yes && !confirm_remove(&name)? {
                println!("Not removed.");
                return Ok(());
            }
            storage::remove(&mut file, &name)?;
            persist(&paths, &file)?;
            println!("Removed {name}");
            Ok(())
        }
        Some(Command::Connect {
            name,
            terminal,
            agent,
        }) => {
            let project = find_project(&file, &name)?;
            let terminal = terminal.unwrap_or(file.settings.terminal);
            if let Some(agent) = agent {
                open_latest_agent_session(&paths, project, agent, terminal)
            } else {
                ssh::connect_with_terminal(project, terminal, Agent::Claude)
            }
        }
        Some(Command::Session { command }) => run_session_command(&paths, &file, command),
        Some(Command::Doctor { shell }) => {
            let shells = ssh::selected_shells(&shell)?;
            let checks = ssh::doctor(&paths, &file, &shells);
            let mut healthy = true;
            for (ok, message) in checks {
                println!("{} {message}", if ok { "[ok]" } else { "[fail]" });
                healthy &= ok;
            }
            if healthy {
                Ok(())
            } else {
                bail!("one or more checks failed")
            }
        }
        Some(Command::IsProject { name }) => {
            if file.projects.iter().any(|project| project.name == name) {
                Ok(())
            } else {
                std::process::exit(1);
            }
        }
        Some(Command::CompleteProjects) => {
            print!("{}", project_completion_output(&file));
            Ok(())
        }
    }
}

fn run_session_command(
    paths: &AppPaths,
    file: &model::ProjectFile,
    command: SessionCommand,
) -> Result<()> {
    match command {
        SessionCommand::List { name } => {
            let project = find_project(file, &name)?;
            let active = ssh::active_session_windows(project).unwrap_or_default();
            let sessions = storage::load_sessions(paths)?;
            for session in sessions
                .sessions
                .iter()
                .filter(|item| item.project == project.name)
            {
                let state = if active.contains(&session.window) {
                    "active"
                } else {
                    "saved"
                };
                println!(
                    "{:<10} {:<8} {:<10} {}",
                    session.label,
                    session.agent.label(),
                    state,
                    session.color
                );
            }
            Ok(())
        }
        SessionCommand::New { name, label, agent } => {
            let project = find_project(file, &name)?;
            let session =
                ssh::create_agent_session(project, agent, &Uuid::new_v4().to_string(), &label)?;
            storage::record_session(paths, session.clone())?;
            ssh::connect_session(project, &session, file.settings.terminal)
        }
        SessionCommand::Open {
            name,
            session,
            all,
            terminal,
        } => {
            let project = find_project(file, &name)?;
            let terminal = terminal.unwrap_or(file.settings.terminal);
            if all {
                return ssh::connect_workspace(project, terminal);
            }
            let selector = session.context("provide a session label or use --all")?;
            let registry = storage::load_sessions(paths)?;
            let record = registry
                .sessions
                .iter()
                .find(|item| {
                    item.project == project.name && (item.label == selector || item.id == selector)
                })
                .with_context(|| {
                    format!(
                        "session {selector:?} does not exist for project {:?}",
                        project.name
                    )
                })?;
            open_saved_session(project, record, terminal)
        }
    }
}

fn open_latest_agent_session(
    paths: &AppPaths,
    project: &Project,
    agent: Agent,
    terminal: TerminalMode,
) -> Result<()> {
    let registry = storage::load_sessions(paths)?;
    let active = ssh::active_session_windows(project)?;
    let record = registry.sessions.iter().rev().find(|item| item.project == project.name && item.agent == agent && active.contains(&item.window))
        .with_context(|| format!("no active {} session for {}; create one with `easy-ssh session new {} <name> --agent {}`", agent.label(), project.name, project.name, agent.label().to_ascii_lowercase()))?;
    ssh::connect_session(project, record, terminal)
}

fn open_saved_session(
    project: &Project,
    record: &storage::SessionRecord,
    terminal: TerminalMode,
) -> Result<()> {
    if ssh::active_session_windows(project)?.contains(&record.window) {
        ssh::connect_session(project, record, terminal)
    } else {
        ssh::resume_agent_session(project, record)?;
        ssh::connect_session(project, record, terminal)
    }
}

fn project_from_add(args: AddArgs) -> Result<Project> {
    let connection = connection_from_args(
        args.host,
        args.ssh_alias,
        args.user,
        args.port,
        args.identity,
    )?;
    let project = Project {
        name: args.name,
        remote_path: args.path,
        connection,
    };
    project.validate()?;
    Ok(project)
}

fn project_from_edit(file: &model::ProjectFile, args: &EditArgs) -> Result<Project> {
    let old = find_project(file, &args.name)?.clone();
    let switching = args.host.is_some() || args.ssh_alias.is_some();
    let connection = if switching {
        connection_from_args(
            args.host.clone(),
            args.ssh_alias.clone(),
            args.user.clone(),
            args.port,
            args.identity.clone(),
        )?
    } else {
        match old.connection {
            Connection::NewHost {
                host,
                user,
                port,
                identity_file,
            } => Connection::NewHost {
                host,
                user: args.user.clone().or(user),
                port: args.port.or(port),
                identity_file: args.identity.clone().or(identity_file),
            },
            Connection::ExistingAlias { ssh_alias } => {
                if args.user.is_some() || args.port.is_some() || args.identity.is_some() {
                    bail!("--user, --port, and --identity require --host for an alias project");
                }
                Connection::ExistingAlias { ssh_alias }
            }
        }
    };
    let project = Project {
        name: args.new_name.clone().unwrap_or(old.name),
        remote_path: args.path.clone().unwrap_or(old.remote_path),
        connection,
    };
    project.validate()?;
    Ok(project)
}

fn connection_from_args(
    host: Option<String>,
    ssh_alias: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity: Option<PathBuf>,
) -> Result<Connection> {
    match (host, ssh_alias) {
        (Some(host), None) => Ok(Connection::NewHost {
            host,
            user,
            port,
            identity_file: identity,
        }),
        (None, Some(ssh_alias)) => {
            if user.is_some() || port.is_some() || identity.is_some() {
                bail!("--user, --port, and --identity can only be used with --host");
            }
            Ok(Connection::ExistingAlias { ssh_alias })
        }
        (None, None) => bail!("provide either --host or --ssh-alias"),
        (Some(_), Some(_)) => unreachable!("clap rejects conflicting connection arguments"),
    }
}

fn find_project<'a>(file: &'a model::ProjectFile, name: &str) -> Result<&'a Project> {
    file.projects
        .iter()
        .find(|project| project.name == name)
        .with_context(|| format!("project {name:?} does not exist"))
}

fn project_completion_output(file: &model::ProjectFile) -> String {
    file.projects
        .iter()
        .map(|project| project.name.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        + if file.projects.is_empty() { "" } else { "\n" }
}

fn persist(paths: &AppPaths, file: &model::ProjectFile) -> Result<()> {
    storage::save(paths, file)?;
    ssh::sync_generated_config(paths, file)
}

fn confirm_remove(name: &str) -> Result<bool> {
    print!("Remove project {name:?}? [y/N] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_completion_output_lists_saved_project_names() {
        let file = model::ProjectFile {
            version: 1,
            projects: vec![
                Project {
                    name: "api".into(),
                    remote_path: "/srv/api".into(),
                    connection: Connection::ExistingAlias {
                        ssh_alias: "production".into(),
                    },
                },
                Project {
                    name: "app".into(),
                    remote_path: "/srv/app".into(),
                    connection: Connection::ExistingAlias {
                        ssh_alias: "staging".into(),
                    },
                },
            ],
            settings: Default::default(),
        };

        assert_eq!(project_completion_output(&file), "api\napp\n");
    }

    #[test]
    fn add_requires_exactly_one_connection_source() {
        let error = connection_from_args(None, None, None, None, None).unwrap_err();
        assert!(error.to_string().contains("either --host or --ssh-alias"));
    }

    #[test]
    fn alias_rejects_new_host_options() {
        assert!(
            connection_from_args(None, Some("prod".into()), Some("deploy".into()), None, None)
                .is_err()
        );
    }
}
