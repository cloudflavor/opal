use crate::{GitLabRemoteConfig, env, runtime};
use anyhow::{Context, Result, anyhow, bail};
use globset::Glob;
use serde_yaml::{Mapping, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::merge_mappings;

pub(super) struct IncludeResolver<'a> {
    include_root: &'a Path,
    include_env: &'a HashMap<String, String>,
    gitlab: Option<&'a GitLabRemoteConfig>,
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
        }
    }

    pub(super) fn load(&self, path: &Path) -> Result<Mapping> {
        let mut stack = Vec::new();
        let context = IncludeContext::local(self.include_root.to_path_buf());
        self.load_pipeline_file(path, &context, &mut stack)
    }

    fn load_pipeline_file(
        &self,
        path: &Path,
        context: &IncludeContext,
        stack: &mut Vec<PathBuf>,
    ) -> Result<Mapping> {
        if stack.iter().any(|entry| entry == path) {
            bail!("include cycle detected involving {:?}", path);
        }
        stack.push(path.to_path_buf());

        let content =
            fs::read_to_string(path).with_context(|| format!("failed to read {:?}", path))?;
        let mut root: Mapping = serde_yaml::from_str(&content)?;
        let include_key = Value::String("include".to_string());
        let mut combined = Mapping::new();

        if let Some(include_value) = root.remove(&include_key) {
            let includes = parse_include_entries(include_value)?;
            for include in includes {
                match include {
                    IncludeEntry::Local(path) => {
                        let include = expand_include_path(&path, self.include_env);
                        for resolved in self.resolve_include_paths(context, &include)? {
                            validate_include_extension(&resolved)?;
                            let canonical = fs::canonicalize(&resolved).with_context(|| {
                                format!("failed to resolve include {:?}", resolved)
                            })?;
                            let included = self.load_pipeline_file(&canonical, context, stack)?;
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
                        let project_root =
                            project_include_root(&runtime::cache_root(), &project, &resolved_ref);
                        let project_context = IncludeContext::for_project(
                            project_root,
                            ProjectIncludeContext {
                                gitlab: gitlab.clone(),
                                project: project.clone(),
                                reference: resolved_ref.clone(),
                            },
                        );
                        for file in files {
                            let fetched = fetch_project_include_file(
                                gitlab,
                                project_context.include_root(),
                                &project,
                                &resolved_ref,
                                &file,
                                self.include_env,
                            )?;
                            let canonical = fs::canonicalize(&fetched).with_context(|| {
                                format!("failed to resolve include {:?}", fetched)
                            })?;
                            let included =
                                self.load_pipeline_file(&canonical, &project_context, stack)?;
                            combined = merge_mappings(combined, included);
                        }
                    }
                }
            }
        }

        combined = merge_mappings(combined, root);
        stack.pop();
        Ok(combined)
    }

    fn resolve_include_paths(
        &self,
        context: &IncludeContext,
        include: &Path,
    ) -> Result<Vec<PathBuf>> {
        if !include_has_glob(include) {
            let resolved = resolve_include_path(context.include_root(), include);
            if resolved.exists() || context.project().is_none() {
                return Ok(vec![resolved]);
            }
            let Some(project_context) = context.project() else {
                return Ok(vec![resolved]);
            };
            let fetched = fetch_project_include_file(
                &project_context.gitlab,
                context.include_root(),
                &project_context.project,
                &project_context.reference,
                include,
                self.include_env,
            )?;
            return Ok(vec![fetched]);
        }

        let pattern = include_glob_pattern(include);
        let matcher = Glob::new(&pattern)
            .with_context(|| format!("invalid include glob '{pattern}'"))?
            .compile_matcher();
        let mut matches = Vec::new();
        for entry in walkdir::WalkDir::new(context.include_root()).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(context.include_root())
                .unwrap_or(entry.path());
            if matcher.is_match(rel) {
                matches.push(entry.path().to_path_buf());
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
        Value::Mapping(map) => {
            let local_key = Value::String("local".to_string());
            let file_key = Value::String("file".to_string());
            let files_key = Value::String("files".to_string());
            let project_key = Value::String("project".to_string());
            let remote_key = Value::String("remote".to_string());
            let template_key = Value::String("template".to_string());
            let component_key = Value::String("component".to_string());
            if map.contains_key(&remote_key) {
                bail!("include:remote is not supported yet");
            }
            if map.contains_key(&template_key) {
                bail!("include:template is not supported yet");
            }
            if map.contains_key(&component_key) {
                bail!("include:component is not supported yet");
            }
            if let Some(Value::String(local)) = map.get(&local_key) {
                Ok(vec![IncludeEntry::Local(PathBuf::from(local))])
            } else if let Some(Value::String(project)) = map.get(&project_key) {
                let reference = map
                    .get(Value::String("ref".to_string()))
                    .map(|value| match value {
                        Value::String(text) => Ok(text.clone()),
                        other => bail!("include:project ref must be a string, got {other:?}"),
                    })
                    .transpose()?;
                let files = parse_project_include_files(map.get(&file_key), map.get(&files_key))?;
                Ok(vec![IncludeEntry::Project {
                    project: project.clone(),
                    reference,
                    files,
                }])
            } else if let Some(Value::String(file)) = map.get(&file_key) {
                Ok(vec![IncludeEntry::Local(PathBuf::from(file))])
            } else if let Some(Value::Sequence(files)) = map.get(&files_key) {
                let mut paths = Vec::new();
                for entry in files {
                    match entry {
                        Value::String(path) => paths.push(IncludeEntry::Local(PathBuf::from(path))),
                        other => bail!("include 'files' entries must be strings, got {other:?}"),
                    }
                }
                Ok(paths)
            } else {
                bail!("only 'local' or 'file(s)' includes are supported");
            }
        }
        other => bail!("unsupported include entry {other:?}"),
    }
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

fn fetch_project_include_file(
    gitlab: &GitLabRemoteConfig,
    project_root: &Path,
    project: &str,
    reference: &str,
    file: &Path,
    include_env: &HashMap<String, String>,
) -> Result<PathBuf> {
    let expanded = expand_include_path(file, include_env);
    validate_include_extension(&expanded)?;
    let relative = expanded.strip_prefix(Path::new("/")).unwrap_or(&expanded);
    let target = project_root.join(relative);
    if target.exists() {
        return Ok(target);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let base = gitlab.base_url.trim_end_matches('/');
    let project_id = percent_encode(project);
    let file_id = percent_encode(&relative.to_string_lossy());
    let ref_id = percent_encode(reference);
    let url =
        format!("{base}/api/v4/projects/{project_id}/repository/files/{file_id}/raw?ref={ref_id}");
    let status = std::process::Command::new("curl")
        .arg("--fail")
        .arg("-sS")
        .arg("-L")
        .arg("-H")
        .arg(format!("PRIVATE-TOKEN: {}", gitlab.token))
        .arg("-o")
        .arg(&target)
        .arg(&url)
        .status()
        .with_context(|| "failed to invoke curl to resolve include:project")?;
    if !status.success() {
        return Err(anyhow!(
            "curl failed to resolve include:project from {} (status {})",
            url,
            status.code().unwrap_or(-1)
        ));
    }
    Ok(target)
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
