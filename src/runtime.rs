use crate::naming::project_slug;
use dirs::{config_dir, data_dir, home_dir};
use std::path::{Path, PathBuf};

const RUNTIME_DIR: &str = ".opal";

fn base_runtime_dir(workdir: &Path) -> PathBuf {
    if let Some(home) = home_dir() {
        return home.join(RUNTIME_DIR);
    }
    if let Some(data) = data_dir() {
        return data.join(RUNTIME_DIR);
    }
    workdir.join(RUNTIME_DIR)
}

fn project_component(workdir: &Path) -> String {
    project_slug(&workdir.to_string_lossy())
}

pub fn runtime_root(workdir: &Path) -> PathBuf {
    base_runtime_dir(workdir).join(project_component(workdir))
}

pub fn session_dir(workdir: &Path, run_id: &str) -> PathBuf {
    runtime_root(workdir).join(run_id)
}

pub fn logs_dir(workdir: &Path, run_id: &str) -> PathBuf {
    session_dir(workdir, run_id).join("logs")
}

pub fn cache_root(workdir: &Path) -> PathBuf {
    runtime_root(workdir).join("cache")
}

pub fn history_path(workdir: &Path) -> PathBuf {
    runtime_root(workdir).join("history.json")
}

pub fn config_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.push(workdir.join(RUNTIME_DIR).join("config.toml"));
    if let Some(mut dir) = config_dir() {
        dir.push("opal");
        dir.push("config.toml");
        paths.push(dir);
    }
    paths
}
