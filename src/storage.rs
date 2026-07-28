use crate::model::{Project, ProjectFile};
use anyhow::{Context, Result, bail};
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    pub registry: PathBuf,
    pub ssh_dir: PathBuf,
    pub ssh_config: PathBuf,
    pub managed_ssh_config: PathBuf,
    pub bashrc: PathBuf,
    pub zshrc: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let config_root = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(Self::for_home_and_config(home, config_root))
    }

    pub fn for_home_and_config(home: PathBuf, config_root: PathBuf) -> Self {
        let config_dir = config_root.join("easy-ssh");
        let ssh_dir = home.join(".ssh");
        Self {
            registry: config_dir.join("projects.toml"),
            ssh_config: ssh_dir.join("config"),
            managed_ssh_config: ssh_dir.join("easy-ssh.conf"),
            bashrc: home.join(".bashrc"),
            zshrc: home.join(".zshrc"),
            home,
            config_dir,
            ssh_dir,
        }
    }
}

pub fn load(paths: &AppPaths) -> Result<ProjectFile> {
    if !paths.registry.exists() {
        return Ok(ProjectFile::default());
    }
    let contents = fs::read_to_string(&paths.registry)
        .with_context(|| format!("failed to read {}", paths.registry.display()))?;
    let file: ProjectFile = toml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", paths.registry.display()))?;
    if file.version != 1 {
        bail!("unsupported project file version {}", file.version);
    }
    for project in &file.projects {
        project
            .validate()
            .with_context(|| format!("invalid project {:?}", project.name))?;
    }
    Ok(file)
}

pub fn save(paths: &AppPaths, file: &ProjectFile) -> Result<()> {
    fs::create_dir_all(&paths.config_dir)
        .with_context(|| format!("failed to create {}", paths.config_dir.display()))?;
    let contents = toml::to_string_pretty(file)?;
    atomic_write(&paths.registry, contents.as_bytes(), 0o600)
}

pub fn upsert(file: &mut ProjectFile, old_name: Option<&str>, project: Project) -> Result<()> {
    project.validate()?;
    if file.projects.iter().any(|candidate| {
        candidate.name == project.name && Some(candidate.name.as_str()) != old_name
    }) {
        bail!("project {:?} already exists", project.name);
    }
    if let Some(old_name) = old_name {
        let slot = file
            .projects
            .iter_mut()
            .find(|candidate| candidate.name == old_name)
            .with_context(|| format!("project {old_name:?} does not exist"))?;
        *slot = project;
    } else {
        file.projects.push(project);
    }
    file.projects
        .sort_by_key(|project| project.name.to_ascii_lowercase());
    Ok(())
}

pub fn remove(file: &mut ProjectFile, name: &str) -> Result<()> {
    let before = file.projects.len();
    file.projects.retain(|project| project.name != name);
    if file.projects.len() == before {
        bail!("project {name:?} does not exist");
    }
    Ok(())
}

pub fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    set_mode(temporary.path(), mode)?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, Connection};
    use tempfile::tempdir;

    fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            remote_path: "/srv/app".into(),
            agent: Agent::Shell,
            connection: Connection::ExistingAlias {
                ssh_alias: "prod".into(),
            },
        }
    }

    #[test]
    fn saves_loads_and_sorts_projects() {
        let temp = tempdir().unwrap();
        let paths = AppPaths::for_home_and_config(temp.path().into(), temp.path().join("xdg"));
        let mut file = ProjectFile::default();
        upsert(&mut file, None, project("zeta")).unwrap();
        upsert(&mut file, None, project("alpha")).unwrap();
        save(&paths, &file).unwrap();
        let loaded = load(&paths).unwrap();
        assert_eq!(loaded.projects[0].name, "alpha");
        assert_eq!(loaded.projects[1].name, "zeta");
    }

    #[test]
    fn upsert_rejects_duplicates() {
        let mut file = ProjectFile::default();
        upsert(&mut file, None, project("alpha")).unwrap();
        assert!(upsert(&mut file, None, project("alpha")).is_err());
    }
}
