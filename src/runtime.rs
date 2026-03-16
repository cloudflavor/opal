use dirs::{config_dir, home_dir};
use std::env;
use std::path::{Path, PathBuf};

const DEFAULT_HOME_DIR: &str = ".opal";
const REPO_CONFIG_DIR: &str = ".opal";

fn opal_home() -> PathBuf {
    if let Some(path) = env::var_os("OPAL_HOME")
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(DEFAULT_HOME_DIR)
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

pub fn config_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.push(workdir.join(REPO_CONFIG_DIR).join("config.toml"));
    paths.push(opal_home().join("config.toml"));
    if let Some(mut dir) = config_dir() {
        dir.push("opal");
        dir.push("config.toml");
        paths.push(dir);
    }
    paths
}
