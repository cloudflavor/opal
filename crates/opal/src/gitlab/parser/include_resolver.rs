use crate::{GitLabRemoteConfig, env, runtime};
use anyhow::{Context, Result, anyhow, bail};
use globset::Glob;
use reqwest::Client;
use serde_yaml::{Mapping, Value, from_str as yaml_from_str};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use tokio::fs;

use super::merge_mappings;

const GITLAB_PRIVATE_TOKEN_HEADER: &str = "PRIVATE-TOKEN";

pub(super) struct IncludeResolver<'a> {
    include_root: &'a Path,
    include_env: &'a HashMap<String, String>,
    gitlab: Option<&'a GitLabRemoteConfig>,
    io: IncludeIo,
}

impl<'a> IncludeResolver<'a> {
    pub(super) fn new(
        include_root: &'a Path,
        include_env: &'a HashMap<String, String>,
        gitlab: Option<&'a GitLabRemoteConfig>,
    ) -> Self {
        Self {
            include_root,
            include_env,
            gitlab,
            io: IncludeIo::new(),
        }
    }

    pub(super) async fn load(&self, path: &Path) -> Result<Mapping> {
        let mut stack = Vec::new();
        let context = IncludeContext::local(self.include_root.to_path_buf());
        self.load_pipeline_file(path, &context, &mut stack).await
    }

    fn load_pipeline_file<'b>(
        &'b self,
        path: &'b Path,
        context: &'b IncludeContext,
        stack: &'b mut Vec<PathBuf>,
    ) -> Pin<Box<dyn Future<Output = Result<Mapping>> + Send + 'b>> {
        Box::pin(async move {
            if stack.iter().any(|entry| entry == path) {
                bail!("include cycle detected involving {:?}", path);
            }
            stack.push(path.to_path_buf());

            let mut root = self.io.read_mapping(path).await?;
            let include_key = Value::String("include".to_string());
            let mut combined = Mapping::new();

            if let Some(include_value) = root.remove(&include_key) {
                let includes = parse_include_entries(include_value)?;
                for include in includes {
                    match include {
                        IncludeEntry::Local(path) => {
                            let include = expand_include_path(&path, self.include_env);
                            for resolved in self.resolve_include_paths(context, &include).await? {
                                validate_include_extension(&resolved)?;
                                let canonical = self.io.canonicalize(&resolved).await?;
                                let included =
                                    self.load_pipeline_file(&canonical, context, stack).await?;
                                combined = merge_mappings(combined, included);
                            }
                        }
                        IncludeEntry::Project {
                            project,
                            reference,
                            files,
                        } => {
                            let gitlab = self.gitlab.ok_or_else(|| {
                                anyhow!(
                                    "include:project requires GitLab credentials/configuration (use --gitlab-token and optionally --gitlab-base-url)"
                                )
                            })?;
                            let resolved_ref = reference.unwrap_or_else(|| "HEAD".to_string());
                            let project_root = project_include_root(
                                &runtime::cache_root(),
                                &project,
                                &resolved_ref,
                            );
                            let project_context = IncludeContext::for_project(
                                project_root,
                                ProjectIncludeContext {
                                    gitlab: gitlab.clone(),
                                    project: project.clone(),
                                    reference: resolved_ref.clone(),
                                },
                            );
                            for file in files {
                                let fetched = self
                                    .materialize_project_include_file(
                                        gitlab,
                                        project_context.include_root(),
                                        &project,
                                        &resolved_ref,
                                        &file,
                                    )
                                    .await?;
                                let canonical = self.io.canonicalize(&fetched).await?;
                                let included = self
                                    .load_pipeline_file(&canonical, &project_context, stack)
                                    .await?;
                                combined = merge_mappings(combined, included);
                            }
                        }
                    }
                }
            }

            combined = merge_mappings(combined, root);
            stack.pop();
            Ok(combined)
        })
    }

    async fn resolve_include_paths(
        &self,
        context: &IncludeContext,
        include: &Path,
    ) -> Result<Vec<PathBuf>> {
        if !include_has_glob(include) {
            let resolved = resolve_include_path(context.include_root(), include);
            if self.io.try_exists(&resolved).await? || context.project().is_none() {
                return Ok(vec![resolved]);
            }
            let Some(project_context) = context.project() else {
                return Ok(vec![resolved]);
            };
            let fetched = self
                .materialize_project_include_file(
                    &project_context.gitlab,
                    context.include_root(),
                    &project_context.project,
                    &project_context.reference,
                    include,
                )
                .await?;
            return Ok(vec![fetched]);
        }

        let pattern = include_glob_pattern(include);
        let matcher = Glob::new(&pattern)
            .with_context(|| format!("invalid include glob '{pattern}'"))?
            .compile_matcher();
        let mut matches = Vec::new();
        for path in self.io.list_files(context.include_root()).await? {
            let rel = path.strip_prefix(context.include_root()).unwrap_or(&path);
            if matcher.is_match(rel) {
                matches.push(path);
            }
        }
        matches.sort();
        matches.dedup();
        if matches.is_empty() && context.project().is_some() {
            bail!("wildcard local includes inside include:project are not supported yet");
        }
        if matches.is_empty() {
            bail!("include glob '{pattern}' matched no files");
        }
        Ok(matches)
    }

    async fn materialize_project_include_file(
        &self,
        gitlab: &GitLabRemoteConfig,
        project_root: &Path,
        project: &str,
        reference: &str,
        file: &Path,
    ) -> Result<PathBuf> {
        let expanded = expand_include_path(file, self.include_env);
        validate_include_extension(&expanded)?;
        let relative = expanded.strip_prefix(Path::new("/")).unwrap_or(&expanded);
        let target = project_root.join(relative);
        if self.io.try_exists(&target).await? {
            return Ok(target);
        }
        self.io.ensure_parent_dir(&target).await?;
        let bytes = self
            .io
            .fetch_project_file(gitlab, project, reference, relative)
            .await?;
        self.io.write(&target, &bytes).await?;
        Ok(target)
    }
}

#[derive(Clone)]
struct IncludeContext {
    include_root: PathBuf,
    project: Option<ProjectIncludeContext>,
}

impl IncludeContext {
    fn local(include_root: PathBuf) -> Self {
        Self {
            include_root,
            project: None,
        }
    }

    fn for_project(include_root: PathBuf, project: ProjectIncludeContext) -> Self {
        Self {
            include_root,
            project: Some(project),
        }
    }

    fn include_root(&self) -> &Path {
        &self.include_root
    }

    fn project(&self) -> Option<&ProjectIncludeContext> {
        self.project.as_ref()
    }
}

struct IncludeIo {
    client: Client,
}

impl IncludeIo {
    fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    async fn read_mapping(&self, path: &Path) -> Result<Mapping> {
        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read {:?}", path))?;
        yaml_from_str(&content).with_context(|| format!("failed to parse {:?}", path))
    }

    async fn canonicalize(&self, path: &Path) -> Result<PathBuf> {
        fs::canonicalize(path)
            .await
            .with_context(|| format!("failed to resolve {:?}", path))
    }

    async fn try_exists(&self, path: &Path) -> Result<bool> {
        fs::try_exists(path)
            .await
            .with_context(|| format!("failed to stat {:?}", path))
    }

    async fn ensure_parent_dir(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        Ok(())
    }

    async fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        fs::write(path, bytes)
            .await
            .with_context(|| format!("failed to write {}", path.display()))
    }

    async fn list_files(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(dir) = pending.pop() {
            let mut entries = fs::read_dir(&dir)
                .await
                .with_context(|| format!("failed to read {}", dir.display()))?;
            while let Some(entry) = entries
                .next_entry()
                .await
                .with_context(|| format!("failed to iterate {}", dir.display()))?
            {
                let path = entry.path();
                let file_type = entry
                    .file_type()
                    .await
                    .with_context(|| format!("failed to inspect {}", path.display()))?;
                if file_type.is_dir() {
                    pending.push(path);
                } else if file_type.is_file() {
                    files.push(path);
                }
            }
        }
        Ok(files)
    }

    async fn fetch_project_file(
        &self,
        gitlab: &GitLabRemoteConfig,
        project: &str,
        reference: &str,
        relative: &Path,
    ) -> Result<Vec<u8>> {
        let url = gitlab_repository_file_raw_url(&gitlab.base_url, project, reference, relative);
        let response = self
            .client
            .get(&url)
            .header(GITLAB_PRIVATE_TOKEN_HEADER, gitlab.token.as_str())
            .send()
            .await
            .with_context(|| format!("failed to request include:project from {}", url))?
            .error_for_status()
            .map_err(|err| {
                anyhow!(
                    "request failed to resolve include:project from {} ({err})",
                    url
                )
            })?;
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("failed to read include:project body from {}", url))?;
        Ok(bytes.to_vec())
    }
}

fn resolve_include_path(include_root: &Path, include: &Path) -> PathBuf {
    if let Ok(stripped) = include.strip_prefix(Path::new("/")) {
        include_root.join(stripped)
    } else if include.is_absolute() {
        include.to_path_buf()
    } else {
        include_root.join(include)
    }
}

fn expand_include_path(include: &Path, include_env: &HashMap<String, String>) -> PathBuf {
    let expanded = env::expand_value(&include.to_string_lossy(), include_env);
    PathBuf::from(expanded)
}

#[derive(Debug, Clone)]
struct ProjectIncludeContext {
    gitlab: GitLabRemoteConfig,
    project: String,
    reference: String,
}

fn include_has_glob(include: &Path) -> bool {
    include
        .to_string_lossy()
        .chars()
        .any(|ch| matches!(ch, '*' | '?' | '['))
}

fn include_glob_pattern(include: &Path) -> String {
    include
        .strip_prefix(Path::new("/"))
        .unwrap_or(include)
        .to_string_lossy()
        .to_string()
}

fn validate_include_extension(path: &Path) -> Result<()> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("yml") | Some("yaml") => Ok(()),
        _ => bail!(
            "include path '{}' must reference a .yml or .yaml file",
            path.display()
        ),
    }
}

fn parse_include_entries(value: Value) -> Result<Vec<IncludeEntry>> {
    match value {
        Value::String(path) => Ok(vec![IncludeEntry::Local(PathBuf::from(path))]),
        Value::Sequence(entries) => {
            let mut paths = Vec::new();
            for entry in entries {
                paths.extend(parse_include_entry(entry)?);
            }
            Ok(paths)
        }
        Value::Mapping(_) => parse_include_entry(value),
        other => bail!("include must be a string or list, got {other:?}"),
    }
}

#[derive(Debug, Clone)]
enum IncludeEntry {
    Local(PathBuf),
    Project {
        project: String,
        reference: Option<String>,
        files: Vec<PathBuf>,
    },
}

fn parse_include_entry(value: Value) -> Result<Vec<IncludeEntry>> {
    match value {
        Value::String(path) => Ok(vec![IncludeEntry::Local(PathBuf::from(path))]),
        Value::Mapping(map) => parse_include_mapping(map),
        other => bail!("unsupported include entry {other:?}"),
    }
}

fn parse_include_mapping(map: Mapping) -> Result<Vec<IncludeEntry>> {
    reject_unsupported_include_sources(&map)?;

    let local_key = include_mapping_key("local");
    let file_key = include_mapping_key("file");
    let files_key = include_mapping_key("files");
    let project_key = include_mapping_key("project");

    if let Some(Value::String(local)) = map.get(&local_key) {
        return Ok(vec![IncludeEntry::Local(PathBuf::from(local))]);
    }

    if let Some(Value::String(project)) = map.get(&project_key) {
        return Ok(vec![IncludeEntry::Project {
            project: project.clone(),
            reference: parse_project_include_reference(&map)?,
            files: parse_project_include_files(map.get(&file_key), map.get(&files_key))?,
        }]);
    }

    if let Some(Value::String(file)) = map.get(&file_key) {
        return Ok(vec![IncludeEntry::Local(PathBuf::from(file))]);
    }

    if let Some(Value::Sequence(files)) = map.get(&files_key) {
        return parse_local_include_files(files);
    }

    bail!("only 'local' or 'file(s)' includes are supported");
}

fn reject_unsupported_include_sources(map: &Mapping) -> Result<()> {
    for key in map.keys().filter_map(Value::as_str) {
        if matches!(key, "remote" | "template" | "component") {
            bail!("include:{key} is not supported yet");
        }
    }
    Ok(())
}

fn parse_project_include_reference(map: &Mapping) -> Result<Option<String>> {
    map.get(include_mapping_key("ref"))
        .map(|value| match value {
            Value::String(text) => Ok(text.clone()),
            other => bail!("include:project ref must be a string, got {other:?}"),
        })
        .transpose()
}

fn parse_local_include_files(files: &[Value]) -> Result<Vec<IncludeEntry>> {
    files
        .iter()
        .map(|entry| match entry {
            Value::String(path) => Ok(IncludeEntry::Local(PathBuf::from(path))),
            other => bail!("include 'files' entries must be strings, got {other:?}"),
        })
        .collect()
}

fn parse_project_include_files(
    file_value: Option<&Value>,
    files_value: Option<&Value>,
) -> Result<Vec<PathBuf>> {
    if files_value.is_some() {
        bail!("include:project must use 'file', not 'files'");
    }
    let Some(value) = file_value else {
        bail!("include:project requires a 'file' entry");
    };
    match value {
        Value::String(path) => Ok(vec![PathBuf::from(path)]),
        Value::Sequence(entries) => entries
            .iter()
            .map(|entry| match entry {
                Value::String(path) => Ok(PathBuf::from(path)),
                other => bail!("include:project file entries must be strings, got {other:?}"),
            })
            .collect(),
        other => bail!("include:project file must be a string or list, got {other:?}"),
    }
}

fn project_include_root(cache_root: &Path, project: &str, reference: &str) -> PathBuf {
    cache_root
        .join("includes")
        .join(percent_encode(project))
        .join(sanitize_reference(reference))
}

fn include_mapping_key(key: &str) -> Value {
    Value::String(key.to_string())
}

fn gitlab_repository_file_raw_url(
    base_url: &str,
    project: &str,
    reference: &str,
    relative: &Path,
) -> String {
    let base = base_url.trim_end_matches('/');
    let project_id = percent_encode(project);
    let file_id = percent_encode(&relative.to_string_lossy());
    let ref_id = percent_encode(reference);
    format!("{base}/api/v4/projects/{project_id}/repository/files/{file_id}/raw?ref={ref_id}")
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

fn sanitize_reference(reference: &str) -> String {
    let mut slug = String::new();
    for ch in reference.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | '.') {
            slug.push(ch);
        } else {
            slug.push('-');
        }
    }
    if slug.is_empty() {
        slug.push_str("ref");
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::{gitlab_repository_file_raw_url, parse_include_entry};
    use serde_yaml::Value;

    #[test]
    fn gitlab_repository_file_raw_url_matches_repository_files_api_shape() {
        let url = gitlab_repository_file_raw_url(
            "https://gitlab.example.com/",
            "group/project",
            "main",
            std::path::Path::new("ci/includes/build.yml"),
        );

        assert_eq!(
            url,
            "https://gitlab.example.com/api/v4/projects/group%2Fproject/repository/files/ci%2Fincludes%2Fbuild.yml/raw?ref=main"
        );
    }

    #[test]
    fn include_project_requires_file_not_files() {
        let value: Value = serde_yaml::from_str(
            r#"
project: group/project
files:
  - ci/includes/build.yml
"#,
        )
        .expect("valid include mapping");

        let err = parse_include_entry(value).expect_err("include:project must reject files");
        assert!(
            err.to_string()
                .contains("include:project must use 'file', not 'files'")
        );
    }
}
