use dirs::{config_dir, data_dir, home_dir};
use std::env;
use std::path::{Path, PathBuf};

const DEFAULT_STATE_DIR: &str = ".local/share/opal";
const DEFAULT_CONFIG_DIR: &str = ".config/opal";
const REPO_CONFIG_DIR: &str = ".opal";

fn default_state_root() -> PathBuf {
    if let Some(dir) = data_dir() {
        return dir.join("opal");
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_STATE_DIR)
}

fn default_config_root() -> PathBuf {
    if let Some(dir) = config_dir() {
        return dir.join("opal");
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_CONFIG_DIR)
}

fn opal_home() -> PathBuf {
    if let Some(path) = env::var_os("OPAL_HOME")
        && !path.is_empty()
    {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return path;
        }
        return env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path);
    }
    default_state_root()
}

pub fn runs_root() -> PathBuf {
    opal_home()
}

pub fn session_dir(run_id: &str) -> PathBuf {
    runs_root().join(run_id)
}

pub fn logs_dir(run_id: &str) -> PathBuf {
    session_dir(run_id).join("logs")
}

pub fn cache_root() -> PathBuf {
    opal_home().join("cache")
}

pub fn history_path() -> PathBuf {
    opal_home().join("history.json")
}

pub fn resource_group_root() -> PathBuf {
    opal_home().join("resource-groups")
}

pub fn config_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.push(workdir.join(REPO_CONFIG_DIR).join("config.toml"));
    paths.push(default_config_root().join("config.toml"));
    if let Some(opal_home_dir) = env::var_os("OPAL_HOME").filter(|p| !p.is_empty()) {
        let path = PathBuf::from(opal_home_dir);
        let config_path = if path.is_absolute() {
            path.join("config.toml")
        } else {
            env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
                .join("config.toml")
        };
        paths.push(config_path);
    }
    paths
}
