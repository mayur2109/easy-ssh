use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub projects: Vec<Project>,
}

impl Default for ProjectFile {
    fn default() -> Self {
        Self {
            version: default_version(),
            projects: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub remote_path: String,
    #[serde(flatten)]
    pub connection: Connection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Connection {
    NewHost {
        host: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        user: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        identity_file: Option<PathBuf>,
    },
    ExistingAlias {
        ssh_alias: String,
    },
}

impl Project {
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        if self.remote_path.trim().is_empty() {
            bail!("remote path cannot be empty");
        }
        if self.remote_path.contains('\n') || self.remote_path.contains('\r') {
            bail!("remote path cannot contain a newline");
        }

        match &self.connection {
            Connection::NewHost {
                host,
                user,
                port,
                identity_file,
            } => {
                validate_single_token("host", host)?;
                if let Some(user) = user {
                    validate_single_token("user", user)?;
                }
                if matches!(port, Some(0)) {
                    bail!("port must be between 1 and 65535");
                }
                if let Some(path) = identity_file {
                    let text = path.to_string_lossy();
                    if text.contains('\n') || text.contains('\r') {
                        bail!("identity file cannot contain a newline");
                    }
                }
            }
            Connection::ExistingAlias { ssh_alias } => {
                validate_name(ssh_alias).map_err(|_| {
                    anyhow::anyhow!(
                        "SSH alias must use only letters, numbers, dot, underscore, or hyphen"
                    )
                })?;
            }
        }
        Ok(())
    }

    pub fn ssh_target(&self) -> &str {
        match &self.connection {
            Connection::NewHost { .. } => &self.name,
            Connection::ExistingAlias { ssh_alias } => ssh_alias,
        }
    }

    pub fn destination(&self) -> String {
        match &self.connection {
            Connection::NewHost {
                host, user, port, ..
            } => {
                let login = user
                    .as_ref()
                    .map(|user| format!("{user}@{host}"))
                    .unwrap_or_else(|| host.clone());
                port.map(|port| format!("{login}:{port}")).unwrap_or(login)
            }
            Connection::ExistingAlias { ssh_alias } => format!("alias:{ssh_alias}"),
        }
    }
}

pub fn validate_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!(
            "project name must use only letters, numbers, dot, underscore, or hyphen and cannot start with a hyphen"
        );
    }
    Ok(())
}

fn validate_single_token(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().any(char::is_whitespace) {
        bail!("{label} cannot be empty or contain whitespace");
    }
    Ok(())
}

const fn default_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_project_names() {
        for valid in ["avarya", "team.prod", "my_project", "web-1"] {
            assert!(validate_name(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "-v", "two words", "bad/name"] {
            assert!(validate_name(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn project_file_round_trips() {
        let file = ProjectFile {
            version: 1,
            projects: vec![Project {
                name: "avarya".into(),
                remote_path: "~/src/avarya".into(),
                connection: Connection::NewHost {
                    host: "server.example.com".into(),
                    user: Some("deploy".into()),
                    port: Some(2222),
                    identity_file: Some(PathBuf::from("~/.ssh/id_ed25519")),
                },
            }],
        };
        let encoded = toml::to_string_pretty(&file).unwrap();
        let decoded: ProjectFile = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, file);
    }

    #[test]
    fn rejects_port_zero() {
        let project = Project {
            name: "avarya".into(),
            remote_path: "/srv/app".into(),
            connection: Connection::NewHost {
                host: "server.example.com".into(),
                user: None,
                port: Some(0),
                identity_file: None,
            },
        };
        assert!(project.validate().is_err());
    }
}
