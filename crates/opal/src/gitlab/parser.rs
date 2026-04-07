mod include_resolver;
mod jobs;
mod normalization;

#[cfg(test)]
mod tests;

use crate::{GitLabRemoteConfig, env, git};
use anyhow::{Context, Result};
use serde_yaml::Mapping;
use std::collections::HashMap;
use std::path::Path;
use tokio::runtime::Runtime;

use super::graph::PipelineGraph;
use include_resolver::IncludeResolver;

impl PipelineGraph {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_path_with_gitlab(path, None)
    }

    pub fn from_path_with_gitlab(
        path: impl AsRef<Path>,
        gitlab: Option<&GitLabRemoteConfig>,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Runtime::new()?.block_on(Self::from_path_with_gitlab_async(path, gitlab))
    }

    pub async fn from_path_async(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_path_with_gitlab_async(path, None).await
    }

    pub async fn from_path_with_gitlab_async(
        path: impl AsRef<Path>,
        gitlab: Option<&GitLabRemoteConfig>,
    ) -> Result<Self> {
        Self::from_path_with_env_async(path, std::env::vars().collect(), gitlab).await
    }

    #[cfg(test)]
    fn from_path_with_env(
        path: impl AsRef<Path>,
        host_env: HashMap<String, String>,
        gitlab: Option<&GitLabRemoteConfig>,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Runtime::new()?.block_on(Self::from_path_with_env_async(path, host_env, gitlab))
    }

    async fn from_path_with_env_async(
        path: impl AsRef<Path>,
        host_env: HashMap<String, String>,
        gitlab: Option<&GitLabRemoteConfig>,
    ) -> Result<Self> {
        let path = path.as_ref();
        let canonical = tokio::fs::canonicalize(path)
            .await
            .with_context(|| format!("failed to resolve {:?}", path))?;
        let include_root = git::repository_root(&canonical)
            .unwrap_or_else(|_| canonical.parent().unwrap_or(Path::new(".")).to_path_buf());
        let include_env = env::build_include_lookup(&canonical, &host_env);
        let root = IncludeResolver::new(&include_root, &include_env, gitlab)
            .load(&canonical)
            .await?;
        let root = normalization::normalize_root(root)?;
        jobs::build_pipeline(root)
    }

    pub fn from_yaml_str(contents: &str) -> Result<Self> {
        let root: Mapping = serde_yaml::from_str(contents)?;
        let root = normalization::normalize_root(root)?;
        jobs::build_pipeline(root)
    }
}

impl std::str::FromStr for PipelineGraph {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_yaml_str(s)
    }
}

pub(super) fn merge_mappings(mut base: Mapping, addition: Mapping) -> Mapping {
    for (key, value) in addition {
        base.insert(key, value);
    }
    base
}
