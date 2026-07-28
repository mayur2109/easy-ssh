mod model;
mod ssh;
mod storage;
mod tui;

use crate::model::{Connection, Project};
use crate::storage::AppPaths;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::io::{self, Write};
use std::path::PathBuf;

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
    Connect { name: String },
    /// Check configuration and shell integration
    Doctor {
        /// Shell startup file to check; repeat to check both
        #[arg(long, value_enum)]
        shell: Vec<ssh::Shell>,
    },
    #[command(name = "__is-project", hide = true)]
    IsProject { name: String },
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
                ssh::connect(project)
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
        Some(Command::Connect { name }) => {
            let project = find_project(&file, &name)?;
            ssh::connect(project)
        }
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
