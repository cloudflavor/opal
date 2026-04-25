use dirs::home_dir;
use std::env;
use std::path::{Path, PathBuf};

const DEFAULT_DATA_HOME_REL: &str = ".local/share";
const DEFAULT_CONFIG_HOME_REL: &str = ".config";
const REPO_CONFIG_DIR: &str = ".opal";

fn user_home_dir() -> PathBuf {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn xdg_data_home() -> PathBuf {
    if let Some(dir) = env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(dir);
    }
    user_home_dir().join(DEFAULT_DATA_HOME_REL)
}

fn xdg_config_home() -> PathBuf {
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(dir);
    }
    user_home_dir().join(DEFAULT_CONFIG_HOME_REL)
}

pub fn runs_root() -> PathBuf {
    xdg_data_home().join("opal")
}

pub fn session_dir(run_id: &str) -> PathBuf {
    runs_root().join(run_id)
}

pub fn logs_dir(run_id: &str) -> PathBuf {
    session_dir(run_id).join("logs")
}

pub fn cache_root() -> PathBuf {
    runs_root().join("cache")
}

pub fn history_path() -> PathBuf {
    runs_root().join("history.json")
}

pub fn resource_group_root() -> PathBuf {
    runs_root().join("resource-groups")
}

pub fn config_dirs(workdir: &Path) -> Vec<PathBuf> {
    vec![
        xdg_config_home().join("opal").join("config.toml"),
        workdir.join(REPO_CONFIG_DIR).join("config.toml"),
    ]
}

#[cfg(test)]
mod tests {
    use super::config_dirs;
    use std::path::Path;

    #[test]
    fn config_dirs_loads_global_then_project() {
        let workdir = Path::new("/tmp/workspace");
        let dirs = config_dirs(workdir);

        assert!(dirs[0].ends_with("opal/config.toml"));
        assert_eq!(dirs.len(), 2);
        assert_eq!(dirs[1], workdir.join(".opal").join("config.toml"));
    }
}
