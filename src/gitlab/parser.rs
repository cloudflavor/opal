// TODO: looks like there's a buttload of crap going on here - one there's a pipeline graph - great
// - bad there's also a parses with sparse parsing function, wtf

use crate::{GitLabRemoteConfig, env, git, runtime};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use globset::Glob;
use humantime;
use petgraph::graph::{DiGraph, NodeIndex};
use serde::Deserialize;
use serde_yaml::value::TaggedValue;
use serde_yaml::{Mapping, Value};
use tracing::warn;

use super::graph::{
    ArtifactConfig, ArtifactWhen, CacheConfig, CacheKey, CachePolicy, DependencySource,
    EnvironmentAction, EnvironmentConfig, ExternalDependency, ImageConfig, Job, JobDependency,
    ParallelConfig, ParallelMatrixEntry, ParallelVariable, PipelineDefaults, PipelineGraph,
    RetryPolicy, ServiceConfig, StageGroup, WorkflowConfig,
};
use super::rules::JobRule;

impl PipelineGraph {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_path_with_gitlab(path, None)
    }

    pub fn from_path_with_gitlab(
        path: impl AsRef<Path>,
        gitlab: Option<&GitLabRemoteConfig>,
    ) -> Result<Self> {
        Self::from_path_with_env(path, std::env::vars().collect(), gitlab)
    }

    fn from_path_with_env(
        path: impl AsRef<Path>,
        host_env: HashMap<String, String>,
        gitlab: Option<&GitLabRemoteConfig>,
    ) -> Result<Self> {
        let path = path.as_ref();
        // TODO: why's there a random file system operation in the middle of some function?
        // move this to it's own thing such that this function accepts path as a param.
        let canonical =
            fs::canonicalize(path).with_context(|| format!("failed to resolve {:?}", path))?;
        let include_root = git::repository_root(&canonical)
            .unwrap_or_else(|_| canonical.parent().unwrap_or(Path::new(".")).to_path_buf());
        let include_env = env::build_include_lookup(&canonical, &host_env);
        let mut stack = Vec::new();
        let root = load_pipeline_file(
            &canonical,
            &include_root,
            &include_env,
            gitlab,
            None,
            &mut stack,
        )?;
        let root = resolve_yaml_merge_keys(resolve_reference_tags(root)?)?;
        Self::from_mapping(root)
    }

    pub fn from_yaml_str(contents: &str) -> Result<Self> {
        let root: Mapping = serde_yaml::from_str(contents)?;
        let root = resolve_yaml_merge_keys(resolve_reference_tags(root)?)?;
        Self::from_mapping(root)
    }

    fn from_mapping(root: Mapping) -> Result<Self> {
        let mut stage_names: Vec<String> = Vec::new();
        let mut defaults = PipelineDefaults::default();
        let mut workflow: Option<WorkflowConfig> = None;
        let mut filters = super::graph::PipelineFilters::default();

        let mut job_defs: HashMap<String, Value> = HashMap::new();
        let mut job_names: Vec<String> = Vec::new();

        for (key, value) in root {
            match key {
                Value::String(name) if name == "stages" => {
                    stage_names = parse_stages(value)?;
                }
                Value::String(name) if name == "cache" => {
                    defaults.cache = parse_cache_value(value)?;
                }
                Value::String(name) if name == "image" => {
                    defaults.image = Some(parse_image(value)?);
                }
                Value::String(name) if name == "variables" => {
                    let vars = parse_variables_map(value)?;
                    defaults.variables.extend(vars);
                }
                Value::String(name) if name == "default" => {
                    parse_default_block(&mut defaults, value)?;
                }
                Value::String(name) if name == "workflow" => {
                    workflow = parse_workflow(value)?;
                }
                Value::String(name) if name == "only" => {
                    filters.only = parse_filter_list(value, "only")?;
                }
                Value::String(name) if name == "except" => {
                    filters.except = parse_filter_list(value, "except")?;
                }
                Value::String(name) => {
                    if is_reserved_keyword(&name) {
                        continue;
                    }

                    match value {
                        Value::Mapping(map) => {
                            job_defs.insert(name.clone(), Value::Mapping(map.clone()));
                            if !name.starts_with('.') {
                                job_names.push(name);
                            }
                        }
                        other => bail!("job '{name}' must be defined as a mapping, got {other:?}"),
                    }
                }
                other => bail!("expected string keys in pipeline, got {other:?}"),
            }
        }

        build_graph(
            defaults,
            workflow,
            filters,
            stage_names,
            job_names,
            job_defs,
        )
    }
}

impl std::str::FromStr for PipelineGraph {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_yaml_str(s)
    }
}

fn load_pipeline_file(
    path: &Path,
    include_root: &Path,
    include_env: &HashMap<String, String>,
    gitlab: Option<&GitLabRemoteConfig>,
    project_context: Option<&ProjectIncludeContext>,
    stack: &mut Vec<PathBuf>,
) -> Result<Mapping> {
    if stack.iter().any(|p| p == path) {
        bail!("include cycle detected involving {:?}", path);
    }
    stack.push(path.to_path_buf());

    let content = fs::read_to_string(path).with_context(|| format!("failed to read {:?}", path))?;
    let mut root: Mapping = serde_yaml::from_str(&content)?;
    let include_key = Value::String("include".to_string());
    let mut combined = Mapping::new();

    // TODO: this does too much - refactor and split accordingly
    if let Some(include_value) = root.remove(&include_key) {
        let includes = parse_include_entries(include_value)?;
        for include in includes {
            match include {
                IncludeEntry::Local(path) => {
                    let include = expand_include_path(&path, include_env);
                    for resolved in
                        resolve_include_paths(include_root, &include, project_context, include_env)?
                    {
                        validate_include_extension(&resolved)?;
                        let canonical = fs::canonicalize(&resolved)
                            .with_context(|| format!("failed to resolve include {:?}", resolved))?;
                        let included = load_pipeline_file(
                            &canonical,
                            include_root,
                            include_env,
                            gitlab,
                            project_context,
                            stack,
                        )?;
                        combined = merge_mappings(combined, included);
                    }
                }
                IncludeEntry::Project {
                    project,
                    reference,
                    files,
                } => {
                    let gitlab = gitlab.ok_or_else(|| {
                        anyhow!(
                            "include:project requires GitLab credentials/configuration (use --gitlab-token and optionally --gitlab-base-url)"
                        )
                    })?;
                    let resolved_ref = reference.unwrap_or_else(|| "HEAD".to_string());
                    let project_root =
                        project_include_root(&runtime::cache_root(), &project, &resolved_ref);
                    let project_context = ProjectIncludeContext {
                        gitlab: gitlab.clone(),
                        project: project.clone(),
                        reference: resolved_ref.clone(),
                    };
                    for file in files {
                        let fetched = fetch_project_include_file(
                            gitlab,
                            &project_root,
                            &project,
                            &resolved_ref,
                            &file,
                            include_env,
                        )?;
                        let included = load_pipeline_file(
                            &fetched,
                            &project_root,
                            include_env,
                            Some(gitlab),
                            Some(&project_context),
                            stack,
                        )?;
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

fn resolve_include_paths(
    include_root: &Path,
    include: &Path,
    project_context: Option<&ProjectIncludeContext>,
    include_env: &HashMap<String, String>,
) -> Result<Vec<PathBuf>> {
    if !include_has_glob(include) {
        let resolved = resolve_include_path(include_root, include);
        if resolved.exists() || project_context.is_none() {
            return Ok(vec![resolved]);
        }
        let Some(project_context) = project_context else {
            return Ok(vec![resolved]);
        };
        let fetched = fetch_project_include_file(
            &project_context.gitlab,
            include_root,
            &project_context.project,
            &project_context.reference,
            include,
            include_env,
        )?;
        return Ok(vec![fetched]);
    }

    let pattern = include_glob_pattern(include);
    let matcher = Glob::new(&pattern)
        .with_context(|| format!("invalid include glob '{pattern}'"))?
        .compile_matcher();
    let mut matches = Vec::new();
    for entry in walkdir::WalkDir::new(include_root).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(include_root)
            .unwrap_or(entry.path());
        if matcher.is_match(rel) {
            matches.push(entry.path().to_path_buf());
        }
    }
    matches.sort();
    matches.dedup();
    if matches.is_empty() && project_context.is_some() {
        bail!("wildcard local includes inside include:project are not supported yet");
    }
    if matches.is_empty() {
        bail!("include glob '{pattern}' matched no files");
    }
    Ok(matches)
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

fn resolve_yaml_merge_keys(root: Mapping) -> Result<Mapping> {
    let resolved = resolve_yaml_merge_value(Value::Mapping(root))?;
    match resolved {
        Value::Mapping(map) => Ok(map),
        other => bail!(
            "pipeline root must be a mapping after resolving YAML merge keys, got {}",
            value_kind(&other)
        ),
    }
}

fn resolve_yaml_merge_value(value: Value) -> Result<Value> {
    match value {
        Value::Mapping(map) => Ok(Value::Mapping(resolve_yaml_merge_mapping(map)?)),
        Value::Sequence(entries) => Ok(Value::Sequence(
            entries
                .into_iter()
                .map(resolve_yaml_merge_value)
                .collect::<Result<Vec<_>>>()?,
        )),
        other => Ok(other),
    }
}

fn resolve_yaml_merge_mapping(map: Mapping) -> Result<Mapping> {
    let merge_key = Value::String("<<".to_string());
    let mut merged = Mapping::new();

    if let Some(merge_value) = map.get(&merge_key).cloned() {
        match resolve_yaml_merge_value(merge_value)? {
            Value::Mapping(parent) => {
                for (key, value) in parent {
                    merged.insert(key, value);
                }
            }
            Value::Sequence(entries) => {
                for entry in entries {
                    let Value::Mapping(parent) = resolve_yaml_merge_value(entry)? else {
                        bail!("YAML merge key expects a mapping or list of mappings");
                    };
                    for (key, value) in parent {
                        merged.insert(key, value);
                    }
                }
            }
            other => {
                bail!(
                    "YAML merge key expects a mapping or list of mappings, got {}",
                    value_kind(&other)
                );
            }
        }
    }

    for (key, value) in map {
        if key == merge_key {
            continue;
        }
        merged.insert(key, resolve_yaml_merge_value(value)?);
    }

    Ok(merged)
}

fn resolve_reference_tags(root: Mapping) -> Result<Mapping> {
    let root_value = Value::Mapping(root);
    let mut visiting = Vec::new();
    let resolved = resolve_references(&root_value, &root_value, &mut visiting)?;
    match resolved {
        Value::Mapping(map) => Ok(map),
        other => bail!(
            "pipeline root must be a mapping after resolving !reference tags, got {}",
            value_kind(&other)
        ),
    }
}

type ReferencePath = Vec<ReferenceSegment>;

#[derive(Clone, PartialEq, Eq)]
enum ReferenceSegment {
    Key(String),
    Index(usize),
}

fn resolve_references(
    value: &Value,
    root: &Value,
    visiting: &mut Vec<ReferencePath>,
) -> Result<Value> {
    match value {
        Value::Tagged(tagged) => {
            if tagged.tag == "reference" {
                let path = parse_reference_path(&tagged.value)?;
                if visiting.iter().any(|current| current == &path) {
                    bail!(
                        "detected recursive !reference {}",
                        describe_reference_path(&path)
                    );
                }
                visiting.push(path.clone());
                let target = follow_reference_path(root, &path).with_context(|| {
                    format!(
                        "failed to resolve !reference {}",
                        describe_reference_path(&path)
                    )
                })?;
                let resolved = resolve_references(target, root, visiting)?;
                visiting.pop();
                Ok(resolved)
            } else {
                let resolved_value = resolve_references(&tagged.value, root, visiting)?;
                Ok(Value::Tagged(Box::new(TaggedValue {
                    tag: tagged.tag.clone(),
                    value: resolved_value,
                })))
            }
        }
        Value::Mapping(map) => {
            let mut resolved = Mapping::with_capacity(map.len());
            for (key, val) in map.iter() {
                let resolved_key = resolve_references(key, root, visiting)?;
                let resolved_val = resolve_references(val, root, visiting)?;
                resolved.insert(resolved_key, resolved_val);
            }
            Ok(Value::Mapping(resolved))
        }
        Value::Sequence(seq) => {
            let mut resolved = Vec::with_capacity(seq.len());
            for entry in seq.iter() {
                resolved.push(resolve_references(entry, root, visiting)?);
            }
            Ok(Value::Sequence(resolved))
        }
        other => Ok(other.clone()),
    }
}

fn parse_reference_path(value: &Value) -> Result<ReferencePath> {
    let entries = match value {
        Value::Sequence(entries) => entries,
        other => bail!(
            "!reference expects a sequence path, got {}",
            value_kind(other)
        ),
    };
    if entries.is_empty() {
        bail!("!reference path must contain at least one entry");
    }
    let mut path = Vec::with_capacity(entries.len());
    for entry in entries.iter() {
        match entry {
            Value::String(name) => path.push(ReferenceSegment::Key(name.clone())),
            Value::Number(number) => {
                let index_u64 = number
                    .as_u64()
                    .ok_or_else(|| anyhow!("!reference indices must be non-negative integers"))?;
                let index = usize::try_from(index_u64).map_err(|_| {
                    anyhow!("!reference index {index_u64} is too large for this platform")
                })?;
                path.push(ReferenceSegment::Index(index));
            }
            other => bail!(
                "!reference path entries must be strings or integers, got {}",
                value_kind(other)
            ),
        }
    }
    Ok(path)
}

fn follow_reference_path<'a>(root: &'a Value, path: &[ReferenceSegment]) -> Result<&'a Value> {
    let mut current = root;
    for segment in path {
        current = match segment {
            ReferenceSegment::Key(name) => {
                let mapping = value_as_mapping(current).ok_or_else(|| {
                    anyhow!(
                        "!reference {} expected a mapping before '{}', found {}",
                        describe_reference_path(path),
                        name,
                        value_kind(current)
                    )
                })?;
                mapping.get(name.as_str()).ok_or_else(|| {
                    anyhow!(
                        "!reference {} key '{}' not found",
                        describe_reference_path(path),
                        name
                    )
                })?
            }
            ReferenceSegment::Index(idx) => {
                let sequence = value_as_sequence(current).ok_or_else(|| {
                    anyhow!(
                        "!reference {} expected a sequence before index {}, found {}",
                        describe_reference_path(path),
                        idx,
                        value_kind(current)
                    )
                })?;
                sequence.get(*idx).ok_or_else(|| {
                    anyhow!(
                        "!reference {} index {} out of bounds (len {})",
                        describe_reference_path(path),
                        idx,
                        sequence.len()
                    )
                })?
            }
        };
    }
    Ok(current)
}

fn describe_reference_path(path: &[ReferenceSegment]) -> String {
    let mut parts = Vec::with_capacity(path.len());
    for segment in path {
        match segment {
            ReferenceSegment::Key(name) => parts.push(name.clone()),
            ReferenceSegment::Index(idx) => parts.push(idx.to_string()),
        }
    }
    format!("[{}]", parts.join(", "))
}

fn value_as_mapping(value: &Value) -> Option<&Mapping> {
    match value {
        Value::Mapping(map) => Some(map),
        Value::Tagged(tagged) => value_as_mapping(&tagged.value),
        _ => None,
    }
}

fn value_as_sequence(value: &Value) -> Option<&Vec<Value>> {
    match value {
        Value::Sequence(seq) => Some(seq),
        Value::Tagged(tagged) => value_as_sequence(&tagged.value),
        _ => None,
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Sequence(_) => "sequence",
        Value::Mapping(_) => "mapping",
        Value::Tagged(tagged) => value_kind(&tagged.value),
    }
}

fn parse_stages(value: Value) -> Result<Vec<String>> {
    match value {
        Value::Sequence(entries) => entries
            .into_iter()
            .map(|val| match val {
                Value::String(name) => Ok(name),
                other => bail!("stage value must be string, got {other:?}"),
            })
            .collect(),
        other => bail!("stages must be a sequence, got {other:?}"),
    }
}

fn parse_default_block(defaults: &mut PipelineDefaults, value: Value) -> Result<()> {
    let mapping = match value {
        Value::Mapping(map) => map,
        other => bail!("default section must be a mapping, got {other:?}"),
    };

    for (key, value) in mapping {
        match key {
            Value::String(name) if name == "before_script" => {
                defaults.before_script = parse_string_list(value, "before_script")?;
            }
            Value::String(name) if name == "after_script" => {
                defaults.after_script = parse_string_list(value, "after_script")?;
            }
            Value::String(name) if name == "image" => {
                defaults.image = Some(parse_image(value)?);
            }
            Value::String(name) if name == "variables" => {
                let vars = parse_variables_map(value)?;
                defaults.variables.extend(vars);
            }
            Value::String(name) if name == "cache" => {
                defaults.cache = parse_cache_value(value)?;
            }
            Value::String(name) if name == "services" => {
                defaults.services = parse_services_value(value, "services")?;
            }
            Value::String(name) if name == "timeout" => {
                defaults.timeout = parse_timeout_value(value, "default.timeout")?;
            }
            Value::String(name) if name == "retry" => {
                let raw: RawRetry =
                    serde_yaml::from_value(value).context("failed to parse default.retry")?;
                defaults.retry = raw.into_policy(&RetryPolicy::default(), "default.retry")?;
            }
            Value::String(name) if name == "interruptible" => {
                defaults.interruptible = extract_bool(value, "default.interruptible")?;
            }
            Value::String(_) => {
                // ignore other default keywords for now
            }
            other => bail!("default keys must be strings, got {other:?}"),
        }
    }

    Ok(())
}

fn parse_workflow(value: Value) -> Result<Option<WorkflowConfig>> {
    let mapping = match value {
        Value::Mapping(map) => map,
        other => bail!("workflow section must be a mapping, got {other:?}"),
    };
    let key = Value::String("rules".to_string());
    let Some(rules_value) = mapping.get(&key) else {
        return Ok(None);
    };
    let rules: Vec<JobRule> =
        serde_yaml::from_value(rules_value.clone()).context("failed to parse workflow.rules")?;
    Ok(Some(WorkflowConfig { rules }))
}

fn is_reserved_keyword(name: &str) -> bool {
    matches!(
        name,
        "stages"
            | "default"
            | "include"
            | "cache"
            | "variables"
            | "workflow"
            | "spec"
            | "image"
            | "services"
            | "before_script"
            | "after_script"
            | "only"
            | "except"
    )
}

fn parse_image(value: Value) -> Result<ImageConfig> {
    match value {
        Value::String(name) => Ok(ImageConfig {
            name,
            docker_platform: None,
            docker_user: None,
            entrypoint: Vec::new(),
        }),
        Value::Mapping(mut map) => {
            if let Some(val) = map.remove(Value::String("name".to_string())) {
                let name = extract_string(val, "image name")?;
                let entrypoint = map
                    .remove(Value::String("entrypoint".to_string()))
                    .map(|value| {
                        serde_yaml::from_value::<ServiceCommand>(value)
                            .map(ServiceCommand::into_vec)
                    })
                    .transpose()
                    .context("failed to parse image.entrypoint")?
                    .unwrap_or_default();
                let docker_cfg = map
                    .remove(Value::String("docker".to_string()))
                    .map(|value| parse_docker_executor_config(value, "image.docker"))
                    .transpose()?;
                Ok(ImageConfig {
                    name,
                    docker_platform: docker_cfg.as_ref().and_then(|cfg| cfg.platform.clone()),
                    docker_user: docker_cfg.and_then(|cfg| cfg.user),
                    entrypoint,
                })
            } else {
                bail!("image mapping must include 'name'")
            }
        }
        other => bail!("image must be a string or mapping, got {other:?}"),
    }
}

struct DockerExecutorConfig {
    platform: Option<String>,
    user: Option<String>,
}

fn parse_docker_executor_config(value: Value, field: &str) -> Result<DockerExecutorConfig> {
    let map = match value {
        Value::Mapping(map) => map,
        other => bail!("{field} must be a mapping, got {other:?}"),
    };
    let platform_key = Value::String("platform".to_string());
    let user_key = Value::String("user".to_string());
    let platform = map
        .get(&platform_key)
        .cloned()
        .map(|value| extract_string(value, &format!("{field}.platform")))
        .transpose()?;
    let user = map
        .get(&user_key)
        .cloned()
        .map(|value| extract_string(value, &format!("{field}.user")))
        .transpose()?;
    if platform.is_none() && user.is_none() {
        bail!("{field} must include 'platform' or 'user'");
    }
    Ok(DockerExecutorConfig { platform, user })
}

fn extract_string(value: Value, what: &str) -> Result<String> {
    match value {
        Value::String(text) => Ok(text),
        other => bail!("{what} must be a string, got {other:?}"),
    }
}

type ParsedJobSpec = (
    RawJob,
    Option<ImageConfig>,
    HashMap<String, String>,
    Vec<CacheConfig>,
    Vec<ServiceConfig>,
    Option<ParallelConfig>,
    Vec<String>,
    Vec<String>,
);

fn parse_job(value: Value) -> Result<ParsedJobSpec> {
    match value {
        Value::Mapping(mut map) => {
            let image_value = map.remove(Value::String("image".to_string()));
            let variables_value = map.remove(Value::String("variables".to_string()));
            let cache_value = map.remove(Value::String("cache".to_string()));
            let services_value = map.remove(Value::String("services".to_string()));
            let parallel_value = map.remove(Value::String("parallel".to_string()));
            let only_value = map.remove(Value::String("only".to_string()));
            let except_value = map.remove(Value::String("except".to_string()));
            let job_spec: RawJob = serde_yaml::from_value(Value::Mapping(map))?;
            let image = image_value.map(parse_image).transpose()?;
            let variables = variables_value
                .map(parse_variables_map)
                .transpose()?
                .unwrap_or_default();
            let cache = cache_value
                .map(parse_cache_value)
                .transpose()?
                .unwrap_or_default();
            let services = services_value
                .map(|value| parse_services_value(value, "services"))
                .transpose()?
                .unwrap_or_default();
            let parallel = parallel_value.map(parse_parallel_value).transpose()?;
            let only = only_value
                .map(|value| parse_filter_list(value, "only"))
                .transpose()?
                .unwrap_or_default();
            let except = except_value
                .map(|value| parse_filter_list(value, "except"))
                .transpose()?
                .unwrap_or_default();
            Ok((
                job_spec, image, variables, cache, services, parallel, only, except,
            ))
        }
        other => bail!("job definition must be a mapping, got {other:?}"),
    }
}

fn parse_string_list(value: Value, field: &str) -> Result<Vec<String>> {
    match value {
        Value::Sequence(entries) => {
            let mut out = Vec::new();
            for entry in entries {
                let text = yaml_command_string(entry)
                    .map_err(|err| anyhow!("{field} entries must be strings ({err})"))?;
                out.push(text);
            }
            Ok(out)
        }
        Value::Null => Ok(Vec::new()),
        other => {
            let text = yaml_command_string(other)
                .map_err(|err| anyhow!("{field} must be a string or sequence ({err})"))?;
            Ok(vec![text])
        }
    }
}

fn parse_cache_value(value: Value) -> Result<Vec<CacheConfig>> {
    match value {
        Value::Sequence(entries) => entries
            .into_iter()
            .map(parse_cache_entry)
            .collect::<Result<Vec<_>>>(),
        Value::Null => Ok(Vec::new()),
        other => Ok(vec![parse_cache_entry(other)?]),
    }
}

fn yaml_command_string(value: Value) -> Result<String, String> {
    match value {
        Value::String(text) => Ok(text),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(boolean) => Ok(boolean.to_string()),
        Value::Null => Ok(String::new()),
        Value::Mapping(map) => mapping_command_string(map),
        other => Err(format!("got {other:?}")),
    }
}

fn mapping_command_string(map: Mapping) -> Result<String, String> {
    if map.len() != 1 {
        return Err(format!(
            "mapping entries must contain exactly one command, got {map:?}"
        ));
    }
    let (key, value) = map
        .into_iter()
        .next()
        .ok_or_else(|| "mapping entries must contain exactly one command".to_string())?;
    let key_text = match key {
        Value::String(text) => text,
        other => return Err(format!("mapping keys must be strings, got {other:?}")),
    };
    let value_text = yaml_command_string(value)?;
    if value_text.is_empty() {
        Ok(format!("{key_text}:"))
    } else {
        Ok(format!("{key_text}: {value_text}"))
    }
}

fn parse_cache_entry(value: Value) -> Result<CacheConfig> {
    let raw: CacheEntryRaw = match value {
        Value::Mapping(_) => serde_yaml::from_value(value)?,
        other => bail!("cache entry must be a mapping, got {other:?}"),
    };
    let key = parse_cache_key(raw.key)?;
    let fallback_keys = raw.fallback_keys;
    let paths = if raw.paths.is_empty() {
        bail!("cache entry must specify at least one path");
    } else {
        raw.paths
    };
    let policy = raw
        .policy
        .as_deref()
        .map(CachePolicy::from_str)
        .unwrap_or(CachePolicy::PullPush);
    Ok(CacheConfig {
        key,
        fallback_keys,
        paths,
        policy,
    })
}

fn parse_cache_key(raw: Option<CacheKeyRaw>) -> Result<CacheKey> {
    let Some(raw) = raw else {
        return Ok(CacheKey::default());
    };

    match raw {
        CacheKeyRaw::Literal(value) => Ok(CacheKey::Literal(value)),
        CacheKeyRaw::Detailed(details) => {
            if details.files.is_empty() {
                bail!("cache key map must include at least one file in 'files'");
            }
            if details.files.len() > 2 {
                bail!("cache key map supports at most two files");
            }
            Ok(CacheKey::Files {
                files: details.files,
                prefix: details.prefix.filter(|value| !value.is_empty()),
            })
        }
    }
}

fn parse_variables_map(value: Value) -> Result<HashMap<String, String>> {
    let mapping = match value {
        Value::Mapping(map) => map,
        other => bail!("variables must be a mapping, got {other:?}"),
    };

    let mut vars = HashMap::new();
    for (key, val) in mapping {
        let name = match key {
            Value::String(s) => s,
            other => bail!("variable names must be strings, got {other:?}"),
        };
        let value = extract_variable_value(val, &format!("variable '{name}'"))?;
        vars.insert(name, value);
    }

    Ok(vars)
}

fn extract_variable_value(value: Value, what: &str) -> Result<String> {
    match value {
        Value::String(text) => Ok(text),
        Value::Bool(flag) => Ok(flag.to_string()),
        Value::Number(num) => Ok(num.to_string()),
        Value::Null => Ok(String::new()),
        Value::Mapping(mut map) => {
            let key = Value::String("value".to_string());
            if let Some(entry) = map.remove(&key) {
                extract_variable_value(entry, what)
            } else {
                bail!("{what} mapping must include 'value'")
            }
        }
        other => bail!("{what} must be a string/bool/number, got {other:?}"),
    }
}

fn parse_services_value(value: Value, field: &str) -> Result<Vec<ServiceConfig>> {
    let entries = match value {
        Value::Sequence(seq) => seq,
        Value::Null => return Ok(Vec::new()),
        other => vec![other],
    };
    let mut services = Vec::new();
    for entry in entries {
        let raw: RawService = serde_yaml::from_value(entry)
            .with_context(|| format!("failed to parse {field} entry"))?;
        let config = match raw {
            RawService::Simple(image) => ServiceConfig {
                image,
                aliases: Vec::new(),
                docker_platform: None,
                docker_user: None,
                entrypoint: Vec::new(),
                command: Vec::new(),
                variables: HashMap::new(),
            },
            RawService::Detailed(details) => details.into_config()?,
        };
        services.push(config);
    }
    Ok(services)
}

fn parse_parallel_value(value: Value) -> Result<ParallelConfig> {
    match value {
        Value::Number(num) => {
            let count = num
                .as_u64()
                .ok_or_else(|| anyhow!("parallel count must be positive integer"))?;
            if count == 0 {
                bail!("parallel count must be greater than zero");
            }
            Ok(ParallelConfig::Count(count.try_into().unwrap_or(u32::MAX)))
        }
        Value::Mapping(mut map) => {
            let matrix_key = Value::String("matrix".to_string());
            let Some(entries) = map.remove(&matrix_key) else {
                bail!("parallel mapping must include 'matrix'");
            };
            let matrices = parse_parallel_matrix(entries)?;
            Ok(ParallelConfig::Matrix(matrices))
        }
        other => bail!("parallel must be an integer or mapping, got {other:?}"),
    }
}

fn parse_parallel_matrix(value: Value) -> Result<Vec<ParallelMatrixEntry>> {
    match value {
        Value::Sequence(entries) => entries
            .into_iter()
            .map(parse_parallel_matrix_entry)
            .collect(),
        other => Ok(vec![parse_parallel_matrix_entry(other)?]),
    }
}

fn parse_parallel_matrix_entry(value: Value) -> Result<ParallelMatrixEntry> {
    let mapping = match value {
        Value::Mapping(map) => map,
        other => bail!("parallel matrix entries must be mappings, got {other:?}"),
    };
    let mut variables = Vec::new();
    for (key, value) in mapping {
        let name = match key {
            Value::String(name) => name,
            other => bail!("parallel matrix variable names must be strings, got {other:?}"),
        };
        let values = match value {
            Value::String(text) => vec![text],
            Value::Sequence(entries) => entries
                .into_iter()
                .map(|entry| match entry {
                    Value::String(text) => Ok(text),
                    other => bail!("parallel matrix values must be strings, got {other:?}"),
                })
                .collect::<Result<Vec<_>>>()?,
            other => bail!("parallel matrix values must be string or list, got {other:?}"),
        };
        if values.is_empty() {
            bail!(
                "parallel matrix variable '{}' must have at least one value",
                name
            );
        }
        variables.push(ParallelVariable { name, values });
    }
    if variables.is_empty() {
        bail!("parallel matrix entries must define at least one variable");
    }
    Ok(ParallelMatrixEntry { variables })
}

fn parse_filter_list(value: Value, field: &str) -> Result<Vec<String>> {
    match value {
        Value::String(text) => Ok(vec![text]),
        Value::Sequence(entries) => entries
            .into_iter()
            .map(|entry| match entry {
                Value::String(text) => Ok(text),
                other => bail!("{field} entries must be strings, got {other:?}"),
            })
            .collect(),
        Value::Mapping(mut map) => {
            let variables_key = Value::String("variables".to_string());
            if let Some(variables) = map.remove(&variables_key) {
                if !map.is_empty() {
                    bail!("{field} mapping supports only 'variables'");
                }
                parse_variable_filter_list(variables, field)
            } else {
                bail!("{field} mapping supports only 'variables'");
            }
        }
        Value::Null => Ok(Vec::new()),
        other => bail!("{field} must be a string or list, got {other:?}"),
    }
}

fn parse_variable_filter_list(value: Value, field: &str) -> Result<Vec<String>> {
    let expressions = match value {
        Value::String(text) => vec![text],
        Value::Sequence(entries) => entries
            .into_iter()
            .map(|entry| match entry {
                Value::String(text) => Ok(text),
                other => bail!("{field}.variables entries must be strings, got {other:?}"),
            })
            .collect::<Result<Vec<_>>>()?,
        Value::Null => Vec::new(),
        other => bail!("{field}.variables must be a string or list, got {other:?}"),
    };

    Ok(expressions
        .into_iter()
        .map(|expr| format!("__opal_variables__:{expr}"))
        .collect())
}

fn parse_timeout_value(value: Value, field: &str) -> Result<Option<Duration>> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => parse_timeout_str(&text, field).map(Some),
        other => bail!("{field} must be a string or null, got {other:?}"),
    }
}

fn parse_optional_timeout(raw: &Option<String>, field: &str) -> Result<Option<Duration>> {
    if let Some(text) = raw {
        Ok(Some(parse_timeout_str(text, field)?))
    } else {
        Ok(None)
    }
}

fn parse_timeout_str(text: &str, field: &str) -> Result<Duration> {
    humantime::parse_duration(text).with_context(|| format!("invalid duration for {field}: {text}"))
}

fn extract_bool(value: Value, field: &str) -> Result<bool> {
    match value {
        Value::Bool(b) => Ok(b),
        other => bail!("{field} must be a boolean, got {other:?}"),
    }
}

// TODO: just no, this does too many things, also it should probably be part of the PipelinGraph,
// since it's... building the pipeline graph.
fn build_graph(
    defaults: PipelineDefaults,
    workflow: Option<WorkflowConfig>,
    filters: super::graph::PipelineFilters,
    stage_names: Vec<String>,
    job_names: Vec<String>,
    job_defs: HashMap<String, Value>,
) -> Result<PipelineGraph> {
    let mut graph = DiGraph::<Job, ()>::new();
    let mut stages: Vec<StageGroup> = stage_names
        .into_iter()
        .map(|name| StageGroup {
            name,
            jobs: Vec::new(),
        })
        .collect();
    let mut name_to_index: HashMap<String, NodeIndex> = HashMap::new();
    let mut pending_needs: Vec<(String, NodeIndex, Vec<JobDependency>)> = Vec::new();

    if stages.is_empty() {
        stages.push(StageGroup {
            name: "default".to_string(),
            jobs: Vec::new(),
        });
    }

    let mut resolved_defs: HashMap<String, Mapping> = HashMap::new();

    for job_name in job_names {
        let merged_map =
            resolve_job_definition(&job_name, &job_defs, &mut resolved_defs, &mut Vec::new())?;
        let (
            job_spec,
            job_image,
            job_variables,
            job_cache,
            job_services,
            job_parallel,
            only,
            except,
        ) = parse_job(Value::Mapping(merged_map))?;
        let inherit_defaults = job_inherit_defaults(&job_spec);
        let stage_name = job_spec.stage.unwrap_or_else(|| {
            stages
                .first()
                .map(|stage| stage.name.as_str())
                .unwrap_or("default")
                .to_string()
        });
        let stage_index = ensure_stage(&mut stages, &stage_name);
        let commands = job_spec.script.into_commands();
        if commands.is_empty() {
            bail!(
                "job '{}' must define a script (directly or via extends)",
                job_name
            );
        }
        let (raw_needs, explicit_needs) = match job_spec.needs {
            Some(entries) => (entries, true),
            None => (Vec::new(), false),
        };
        let needs: Vec<JobDependency> = raw_needs
            .into_iter()
            .filter_map(|need| need.into_dependency(&job_name))
            .collect();
        let dependencies = job_spec.dependencies;
        let before_script = job_spec.before_script.map(Script::into_commands);
        let after_script = job_spec.after_script.map(Script::into_commands);
        let artifacts = job_spec.artifacts.into_config(&job_name)?;
        let cache_entries = if job_cache.is_empty() && inherit_defaults.cache {
            defaults.cache.clone()
        } else {
            job_cache
        };
        let services = if job_services.is_empty() && inherit_defaults.services {
            defaults.services.clone()
        } else {
            job_services
        };
        let timeout =
            parse_optional_timeout(&job_spec.timeout, &format!("job '{}'.timeout", job_name))?.or(
                if inherit_defaults.timeout {
                    defaults.timeout
                } else {
                    None
                },
            );
        let retry_base = if inherit_defaults.retry {
            defaults.retry.clone()
        } else {
            RetryPolicy::default()
        };
        let retry = job_spec
            .retry
            .map(|raw| raw.into_policy(&retry_base, &format!("job '{}'.retry", job_name)))
            .transpose()?
            .unwrap_or(retry_base);
        let interruptible = job_spec
            .interruptible
            .unwrap_or(if inherit_defaults.interruptible {
                defaults.interruptible
            } else {
                false
            });
        let resource_group = job_spec.resource_group.clone();
        let parallel = job_parallel;

        let environment = job_spec.environment.as_ref().map(|env| {
            let action = match env.action.as_deref() {
                Some("prepare") => EnvironmentAction::Prepare,
                Some("stop") => EnvironmentAction::Stop,
                Some("verify") => EnvironmentAction::Verify,
                Some("access") => EnvironmentAction::Access,
                _ => EnvironmentAction::Start,
            };
            let name = if env.name.is_empty() {
                job_name.clone()
            } else {
                env.name.clone()
            };
            EnvironmentConfig {
                name,
                url: env.url.clone(),
                on_stop: env.on_stop.clone(),
                auto_stop_in: parse_optional_timeout(
                    &env.auto_stop_in,
                    &format!("job '{}'.environment.auto_stop_in", job_name),
                )
                .ok()
                .flatten(),
                action,
            }
        });

        let inherited_image = if job_image.is_none() && inherit_defaults.image {
            defaults.image.clone()
        } else {
            job_image
        };

        let node = graph.add_node(Job {
            name: job_name.clone(),
            stage: stage_name,
            commands,
            needs: needs.clone(),
            explicit_needs,
            dependencies: dependencies.clone(),
            before_script,
            after_script,
            inherit_default_image: inherit_defaults.image,
            inherit_default_before_script: inherit_defaults.before_script,
            inherit_default_after_script: inherit_defaults.after_script,
            inherit_default_cache: inherit_defaults.cache,
            inherit_default_services: inherit_defaults.services,
            inherit_default_timeout: inherit_defaults.timeout,
            inherit_default_retry: inherit_defaults.retry,
            inherit_default_interruptible: inherit_defaults.interruptible,
            when: job_spec.when.clone(),
            rules: job_spec.rules.clone(),
            artifacts,
            cache: cache_entries,
            image: inherited_image,
            variables: job_variables,
            services,
            timeout,
            retry,
            interruptible,
            resource_group,
            parallel,
            only,
            except,
            tags: job_spec.tags.clone(),
            environment,
        });

        name_to_index.insert(job_name.clone(), node);
        pending_needs.push((job_name, node, needs));

        let stage = stages
            .get_mut(stage_index)
            .ok_or_else(|| anyhow!("internal error: stage index {} missing", stage_index))?;
        stage.jobs.push(node);
    }

    // TODO; for for for for fuck off - refactor

    for (job_name, job_idx, needs) in pending_needs {
        for dependency in needs {
            if !matches!(dependency.source, DependencySource::Local) {
                continue;
            }
            let Some(dependency_idx) = name_to_index.get(&dependency.job).copied() else {
                if dependency.optional {
                    continue;
                }
                return Err(anyhow::anyhow!(
                    "job '{}' declared unknown dependency '{}'",
                    job_name,
                    dependency.job
                ));
            };

            graph.add_edge(dependency_idx, job_idx, ());
        }
    }

    Ok(PipelineGraph {
        graph,
        stages,
        defaults,
        workflow,
        filters,
    })
}

struct JobInheritDefaults {
    image: bool,
    before_script: bool,
    after_script: bool,
    cache: bool,
    services: bool,
    timeout: bool,
    retry: bool,
    interruptible: bool,
}

impl Default for JobInheritDefaults {
    fn default() -> Self {
        Self {
            image: true,
            before_script: true,
            after_script: true,
            cache: true,
            services: true,
            timeout: true,
            retry: true,
            interruptible: true,
        }
    }
}

fn job_inherit_defaults(job: &RawJob) -> JobInheritDefaults {
    let mut inherit = JobInheritDefaults::default();
    if let Some(raw_inherit) = &job.inherit
        && let Some(default) = &raw_inherit.default
    {
        match default {
            RawInheritDefault::Bool(value) => {
                inherit.image = *value;
                inherit.before_script = *value;
                inherit.after_script = *value;
                inherit.cache = *value;
                inherit.services = *value;
                inherit.timeout = *value;
                inherit.retry = *value;
                inherit.interruptible = *value;
            }
            RawInheritDefault::List(entries) => {
                inherit.image = entries.iter().any(|entry| entry == "image");
                inherit.before_script = entries.iter().any(|entry| entry == "before_script");
                inherit.after_script = entries.iter().any(|entry| entry == "after_script");
                inherit.cache = entries.iter().any(|entry| entry == "cache");
                inherit.services = entries.iter().any(|entry| entry == "services");
                inherit.timeout = entries.iter().any(|entry| entry == "timeout");
                inherit.retry = entries.iter().any(|entry| entry == "retry");
                inherit.interruptible = entries.iter().any(|entry| entry == "interruptible");
            }
        }
    }
    inherit
}

fn resolve_job_definition(
    name: &str,
    job_defs: &HashMap<String, Value>,
    cache: &mut HashMap<String, Mapping>,
    stack: &mut Vec<String>,
) -> Result<Mapping> {
    if let Some(resolved) = cache.get(name) {
        return Ok(resolved.clone());
    }

    if stack.iter().any(|entry| entry == name) {
        bail!("job '{}' has cyclical extends", name);
    }

    let value = match job_defs.get(name) {
        Some(v) => v,
        None => {
            let requester = stack.last().cloned().unwrap_or_else(|| name.to_string());
            bail!("job '{requester}' extends unknown job/template '{name}'");
        }
    };

    let map = match value {
        Value::Mapping(map) => map.clone(),
        other => bail!("job '{name}' must be defined as mapping, got {other:?}"),
    };

    stack.push(name.to_string());

    let extends_key = Value::String("extends".to_string());
    let extends = map.get(&extends_key).map(parse_extends_list).transpose()?;

    let mut merged = Mapping::new();
    if let Some(parents) = extends {
        for parent_name in parents {
            let parent_map = resolve_job_definition(&parent_name, job_defs, cache, stack)?;
            merged = merge_mappings(merged, parent_map);
        }
    }

    let mut child_map = map;
    child_map.remove(&extends_key);
    merged = merge_mappings(merged, child_map);

    stack.pop();
    cache.insert(name.to_string(), merged.clone());
    Ok(merged)
}

fn parse_extends_list(value: &Value) -> Result<Vec<String>> {
    match value {
        Value::String(name) => Ok(vec![name.clone()]),
        Value::Sequence(seq) => seq
            .iter()
            .map(|val| match val {
                Value::String(name) => Ok(name.clone()),
                other => bail!("extends entries must be strings, got {other:?}"),
            })
            .collect(),
        other => bail!("extends must be string or sequence, got {other:?}"),
    }
}

fn merge_mappings(mut base: Mapping, addition: Mapping) -> Mapping {
    for (key, value) in addition {
        base.insert(key, value);
    }
    base
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

// TODO - refactor, does way too much - garbage
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

// TODO: this does so much, it creates new paths, expands, formats hardcoded shit from the API. awful
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

fn ensure_stage(stages: &mut Vec<StageGroup>, stage_name: &str) -> usize {
    if let Some(pos) = stages.iter().position(|stage| stage.name == stage_name) {
        pos
    } else {
        stages.push(StageGroup {
            name: stage_name.to_string(),
            jobs: Vec::new(),
        });
        stages.len() - 1
    }
}

#[derive(Debug, Deserialize)]
struct RawJob {
    #[serde(default)]
    before_script: Option<Script>,
    #[serde(default)]
    after_script: Option<Script>,
    #[serde(default)]
    stage: Option<String>,
    #[serde(default)]
    script: Script,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    needs: Option<Vec<Need>>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    rules: Vec<JobRule>,
    #[serde(default)]
    artifacts: RawArtifacts,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    retry: Option<RawRetry>,
    #[serde(default)]
    interruptible: Option<bool>,
    #[serde(default)]
    resource_group: Option<String>,
    #[serde(default)]
    inherit: Option<RawInherit>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    environment: Option<RawEnvironment>,
}

#[derive(Debug, Deserialize, Default)]
struct RawEnvironment {
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    on_stop: Option<String>,
    #[serde(default)]
    auto_stop_in: Option<String>,
    #[serde(default)]
    action: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
struct Script(StringList);

impl Script {
    fn into_commands(self) -> Vec<String> {
        self.0.into_vec()
    }
}

#[derive(Debug, Deserialize, Default)]
struct RawArtifacts {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    paths: Vec<PathBuf>,
    #[serde(default)]
    exclude: StringList,
    #[serde(default)]
    untracked: bool,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    expire_in: Option<String>,
    #[serde(default)]
    reports: RawArtifactReports,
}

impl RawArtifacts {
    fn into_config(self, job_name: &str) -> Result<ArtifactConfig> {
        validate_artifact_excludes(&self.exclude.0, job_name)?;
        Ok(ArtifactConfig {
            name: self.name,
            paths: self.paths,
            exclude: self.exclude.into_vec(),
            untracked: self.untracked,
            when: parse_artifact_when(self.when.as_deref(), job_name)?,
            expire_in: parse_optional_timeout(
                &self.expire_in,
                &format!("job '{}'.artifacts.expire_in", job_name),
            )?,
            report_dotenv: self.reports.dotenv,
        })
    }
}

#[derive(Debug, Deserialize, Default)]
struct RawArtifactReports {
    #[serde(default)]
    dotenv: Option<PathBuf>,
}

fn validate_artifact_excludes(patterns: &[String], job_name: &str) -> Result<()> {
    for pattern in patterns {
        Glob::new(pattern).with_context(|| {
            format!(
                "job '{}' has invalid artifacts.exclude pattern '{}'",
                job_name, pattern
            )
        })?;
    }
    Ok(())
}

fn parse_artifact_when(value: Option<&str>, job_name: &str) -> Result<ArtifactWhen> {
    match value.unwrap_or("on_success") {
        "on_success" => Ok(ArtifactWhen::OnSuccess),
        "on_failure" => Ok(ArtifactWhen::OnFailure),
        "always" => Ok(ArtifactWhen::Always),
        other => bail!(
            "job '{}' has unsupported artifacts.when value '{}'",
            job_name,
            other
        ),
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawService {
    Simple(String),
    Detailed(Box<RawServiceConfig>),
}

#[derive(Debug, Deserialize)]
struct RawServiceConfig {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    docker: Option<Value>,
    #[serde(default)]
    entrypoint: ServiceCommand,
    #[serde(default)]
    command: ServiceCommand,
    #[serde(default)]
    variables: HashMap<String, String>,
}

impl RawServiceConfig {
    fn into_config(self) -> Result<ServiceConfig> {
        let image = self
            .image
            .or(self.name)
            .ok_or_else(|| anyhow!("service entry must specify an image (name)"))?;
        let docker = self
            .docker
            .map(|value| parse_docker_executor_config(value, "services.docker"))
            .transpose()?;
        Ok(ServiceConfig {
            image,
            aliases: parse_service_aliases(self.alias),
            docker_platform: docker.as_ref().and_then(|cfg| cfg.platform.clone()),
            docker_user: docker.and_then(|cfg| cfg.user),
            entrypoint: self.entrypoint.into_vec(),
            command: self.command.into_vec(),
            variables: self.variables,
        })
    }
}

fn parse_service_aliases(alias: Option<String>) -> Vec<String> {
    alias
        .into_iter()
        .flat_map(|raw| {
            raw.split(',')
                .map(str::trim)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|value| !value.is_empty())
        .collect()
}

#[cfg(test)]
mod service_alias_tests {
    use super::parse_service_aliases;

    #[test]
    fn parse_service_aliases_splits_comma_separated_values() {
        assert_eq!(
            parse_service_aliases(Some("db,postgres,pg".into())),
            vec!["db", "postgres", "pg"]
        );
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawRetry {
    Simple(u32),
    Detailed(RawRetryConfig),
}

impl RawRetry {
    fn into_policy(self, base: &RetryPolicy, field: &str) -> Result<RetryPolicy> {
        match self {
            RawRetry::Simple(max) => {
                validate_retry_max(max, field)?;
                Ok(RetryPolicy {
                    max,
                    when: base.when.clone(),
                    exit_codes: base.exit_codes.clone(),
                })
            }
            RawRetry::Detailed(cfg) => cfg.into_policy(base, field),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawRetryConfig {
    #[serde(default)]
    max: Option<u32>,
    #[serde(default)]
    when: StringList,
    #[serde(default)]
    exit_codes: IntList,
}

impl RawRetryConfig {
    fn into_policy(self, base: &RetryPolicy, field: &str) -> Result<RetryPolicy> {
        let mut policy = base.clone();
        if let Some(max) = self.max {
            validate_retry_max(max, &format!("{field}.max"))?;
            policy.max = max;
        }
        if !self.when.0.is_empty() {
            validate_retry_when(&self.when.0, &format!("{field}.when"))?;
            policy.when = self.when.into_vec();
        }
        if !self.exit_codes.0.is_empty() {
            validate_retry_exit_codes(&self.exit_codes.0, &format!("{field}.exit_codes"))?;
            policy.exit_codes = self.exit_codes.into_vec();
        }
        Ok(policy)
    }
}

fn validate_retry_max(max: u32, field: &str) -> Result<()> {
    if max > 2 {
        bail!("{field} must be 0, 1, or 2");
    }
    Ok(())
}

fn validate_retry_when(conditions: &[String], field: &str) -> Result<()> {
    for condition in conditions {
        if !SUPPORTED_RETRY_WHEN_VALUES.contains(&condition.as_str()) {
            bail!("{field} has unsupported retry condition '{condition}'");
        }
    }
    Ok(())
}

fn validate_retry_exit_codes(codes: &[i32], field: &str) -> Result<()> {
    for code in codes {
        if *code < 0 {
            bail!("{field} must contain non-negative integers");
        }
    }
    Ok(())
}

const SUPPORTED_RETRY_WHEN_VALUES: &[&str] = &[
    "always",
    "unknown_failure",
    "script_failure",
    "api_failure",
    "stuck_or_timeout_failure",
    "runner_system_failure",
    "runner_unsupported",
    "stale_schedule",
    "job_execution_timeout",
    "archived_failure",
    "unmet_prerequisites",
    "scheduler_failure",
    "data_integrity_failure",
];

#[derive(Debug, Default)]
struct StringList(Vec<String>);

impl StringList {
    fn into_vec(self) -> Vec<String> {
        self.0
    }
}

#[derive(Debug, Default)]
struct IntList(Vec<i32>);

impl IntList {
    fn into_vec(self) -> Vec<i32> {
        self.0
    }
}

#[derive(Debug, Default)]
struct ServiceCommand(Vec<String>);

impl ServiceCommand {
    fn into_vec(self) -> Vec<String> {
        self.0
    }
}

impl<'de> serde::Deserialize<'de> for ServiceCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let list = StringList::deserialize(deserializer)?;
        Ok(ServiceCommand(list.into_vec()))
    }
}
impl<'de> serde::Deserialize<'de> for StringList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let items = match value {
            Value::Sequence(entries) => {
                let mut commands = Vec::new();
                for entry in entries {
                    let cmd = yaml_command_string(entry).map_err(serde::de::Error::custom)?;
                    commands.push(cmd);
                }
                commands
            }
            Value::Null => Vec::new(),
            other => vec![yaml_command_string(other).map_err(serde::de::Error::custom)?],
        };
        Ok(StringList(items))
    }
}

impl<'de> serde::Deserialize<'de> for IntList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let items = match value {
            Value::Sequence(entries) => {
                let mut codes = Vec::new();
                for entry in entries {
                    match entry {
                        Value::Number(number) => {
                            let code = number.as_i64().ok_or_else(|| {
                                serde::de::Error::custom("retry exit codes must be integers")
                            })?;
                            let code = i32::try_from(code).map_err(|_| {
                                serde::de::Error::custom(
                                    "retry exit code is too large for this platform",
                                )
                            })?;
                            codes.push(code);
                        }
                        other => {
                            return Err(serde::de::Error::custom(format!(
                                "retry exit codes must be integers, got {other:?}"
                            )));
                        }
                    }
                }
                codes
            }
            Value::Null => Vec::new(),
            Value::Number(number) => {
                let code = number
                    .as_i64()
                    .ok_or_else(|| serde::de::Error::custom("retry exit codes must be integers"))?;
                vec![i32::try_from(code).map_err(|_| {
                    serde::de::Error::custom("retry exit code is too large for this platform")
                })?]
            }
            other => {
                return Err(serde::de::Error::custom(format!(
                    "retry exit codes must be an integer or list, got {other:?}"
                )));
            }
        };
        Ok(IntList(items))
    }
}

#[derive(Debug, Deserialize, Default)]
struct CacheEntryRaw {
    key: Option<CacheKeyRaw>,
    #[serde(default)]
    fallback_keys: Vec<String>,
    #[serde(default)]
    paths: Vec<PathBuf>,
    #[serde(default)]
    policy: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CacheKeyRaw {
    Literal(String),
    Detailed(CacheKeyDetailedRaw),
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct CacheKeyDetailedRaw {
    #[serde(default)]
    files: Vec<PathBuf>,
    #[serde(default)]
    prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Need {
    Name(String),
    Config(NeedConfig),
}

#[derive(Debug, Deserialize)]
struct NeedConfig {
    job: String,
    #[serde(default = "default_artifacts_true")]
    artifacts: bool,
    #[serde(default)]
    optional: bool,
    #[serde(default)]
    project: Option<String>,
    #[serde(rename = "ref")]
    reference: Option<String>,
    #[serde(default)]
    parallel: Option<NeedParallelRaw>,
}

impl Need {
    fn into_dependency(self, owner: &str) -> Option<JobDependency> {
        match self {
            Need::Name(job) => {
                if let Some((base, values)) = parse_inline_variant_reference(&job) {
                    Some(JobDependency {
                        job: base,
                        needs_artifacts: true,
                        optional: false,
                        source: DependencySource::Local,
                        parallel: None,
                        inline_variant: Some(values),
                    })
                } else {
                    Some(JobDependency {
                        job,
                        needs_artifacts: true,
                        optional: false,
                        source: DependencySource::Local,
                        parallel: None,
                        inline_variant: None,
                    })
                }
            }
            Need::Config(cfg) => {
                let NeedConfig {
                    job,
                    artifacts,
                    optional,
                    project,
                    reference,
                    parallel,
                } = cfg;
                if let Some(project) = project {
                    let reference = reference.unwrap_or_default();
                    if reference.is_empty() {
                        warn!(
                            job = owner,
                            dependency = %job,
                            "needs:project for '{}' is missing required 'ref'",
                            project
                        );
                        return None;
                    }
                    Some(JobDependency {
                        job,
                        needs_artifacts: artifacts,
                        optional,
                        source: DependencySource::External(ExternalDependency {
                            project,
                            reference,
                        }),
                        parallel: None,
                        inline_variant: None,
                    })
                } else {
                    let parallel_filters =
                        parallel.and_then(|raw| match raw.into_filters(owner, &job) {
                            Ok(filters) => Some(filters),
                            Err(err) => {
                                warn!(
                                    job = owner,
                                    dependency = %job,
                                    error = %err,
                                    "invalid needs.parallel configuration"
                                );
                                None
                            }
                        });
                    let (job_name, inline_variant) = parse_inline_variant_reference(&job)
                        .map_or((job, None), |(base, values)| (base, Some(values)));
                    Some(JobDependency {
                        job: job_name,
                        needs_artifacts: artifacts,
                        optional,
                        source: DependencySource::Local,
                        parallel: parallel_filters,
                        inline_variant,
                    })
                }
            }
        }
    }
}

fn parse_inline_variant_reference(value: &str) -> Option<(String, Vec<String>)> {
    let trimmed = value.trim();
    let (base, list) = trimmed.split_once(':')?;
    let payload = list.trim();
    if !payload.starts_with('[') {
        return None;
    }
    let values: Vec<String> = serde_yaml::from_str(payload).ok()?;
    Some((base.trim().to_string(), values))
}

#[derive(Debug, Deserialize)]
struct NeedParallelRaw {
    #[serde(default)]
    matrix: Vec<HashMap<String, Value>>,
}

impl NeedParallelRaw {
    // TODO: this function has nested for loops - you're going to get the quadratic award
    fn into_filters(self, owner: &str, dependency: &str) -> Result<Vec<HashMap<String, String>>> {
        let mut filters = Vec::new();
        for entry in self.matrix {
            let mut filter = HashMap::new();
            for (name, value) in entry {
                let value = match value {
                    Value::String(text) => text,
                    other => bail!(
                        "job '{}' dependency '{}' parallel values must be strings, got {other:?}",
                        owner,
                        dependency
                    ),
                };
                filter.insert(name, value);
            }
            if filter.is_empty() {
                bail!(
                    "job '{}' dependency '{}' parallel matrix entries must include variables",
                    owner,
                    dependency
                );
            }
            filters.push(filter);
        }
        if filters.is_empty() {
            bail!(
                "job '{}' dependency '{}' parallel matrix must include at least one entry",
                owner,
                dependency
            );
        }
        Ok(filters)
    }
}

fn default_artifacts_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Result, anyhow};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_stage_and_job_order() -> Result<()> {
        let yaml = r#"
    stages:
      - build
      - test

    build-job:
      stage: build
      script:
        - echo build

    second-build:
      stage: build
      script: echo build 2

    test-job:
      stage: test
      script:
        - echo test
    "#;

        let pipeline = PipelineGraph::from_yaml_str(yaml)?;
        assert_eq!(pipeline.stages.len(), 2);
        assert_eq!(pipeline.stages[0].name, "build");
        assert_eq!(pipeline.stages[1].name, "test");

        let build_jobs: Vec<&Job> = pipeline.stages[0]
            .jobs
            .iter()
            .map(|idx| &pipeline.graph[*idx])
            .collect();
        assert_eq!(build_jobs.len(), 2);
        assert_eq!(build_jobs[0].name, "build-job");
        assert_eq!(build_jobs[1].name, "second-build");

        let test_jobs: Vec<&Job> = pipeline.stages[1]
            .jobs
            .iter()
            .map(|idx| &pipeline.graph[*idx])
            .collect();
        assert_eq!(test_jobs.len(), 1);
        assert_eq!(test_jobs[0].name, "test-job");
        Ok(())
    }

    #[test]
    fn resolves_reference_tags() -> Result<()> {
        let yaml = r#"
    stages: [build]

    .shared:
      script:
        - echo shared
      variables:
        SHARED_VAR: shared-value

    build-job:
      stage: build
      script: !reference [.shared, script]
      variables:
        COPIED: !reference [.shared, variables, SHARED_VAR]
    "#;

        let pipeline = PipelineGraph::from_yaml_str(yaml)?;
        assert_eq!(pipeline.stages.len(), 1);
        let build_stage = &pipeline.stages[0];
        assert_eq!(build_stage.jobs.len(), 1);
        let job = &pipeline.graph[build_stage.jobs[0]];
        assert_eq!(job.commands, vec!["echo shared"]);
        assert_eq!(
            job.variables.get("COPIED").map(|value| value.as_str()),
            Some("shared-value")
        );
        Ok(())
    }

    #[test]
    fn includes_local_fragment() -> Result<()> {
        let dir = tempdir()?;
        let fragment_path = dir.path().join("fragment.yml");
        fs::write(
            &fragment_path,
            r#"
fragment-job:
  stage: build
  script:
    - echo fragment
"#,
        )?;

        let main_path = dir.path().join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build
include:
  - local: fragment.yml

main-job:
  stage: build
  script:
    - echo main
"#,
        )?;

        let pipeline = PipelineGraph::from_path(&main_path)?;
        assert_eq!(pipeline.stages.len(), 1);
        assert_eq!(pipeline.stages[0].jobs.len(), 2);
        assert!(
            pipeline
                .graph
                .node_weights()
                .any(|job| job.name == "fragment-job")
        );
        assert!(
            pipeline
                .graph
                .node_weights()
                .any(|job| job.name == "main-job")
        );
        Ok(())
    }

    #[test]
    fn includes_local_paths_from_repo_root() -> Result<()> {
        let dir = tempdir()?;
        git2::Repository::init(dir.path())?;

        let fragment_path = dir.path().join("fragment.yml");
        fs::write(
            &fragment_path,
            r#"
fragment-job:
  stage: build
  script:
    - echo fragment
"#,
        )?;

        let ci_dir = dir.path().join("ci");
        fs::create_dir_all(&ci_dir)?;
        let main_path = ci_dir.join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build
include:
  - local: fragment.yml

main-job:
  stage: build
  script:
    - echo main
"#,
        )?;

        let pipeline = PipelineGraph::from_path(&main_path)?;
        assert!(
            pipeline
                .graph
                .node_weights()
                .any(|job| job.name == "fragment-job")
        );
        assert!(
            pipeline
                .graph
                .node_weights()
                .any(|job| job.name == "main-job")
        );
        Ok(())
    }

    #[test]
    fn includes_local_glob_from_repo_root() -> Result<()> {
        let dir = tempdir()?;
        git2::Repository::init(dir.path())?;

        let configs_dir = dir.path().join("configs");
        fs::create_dir_all(&configs_dir)?;
        fs::write(
            configs_dir.join("a.yml"),
            r#"
job-a:
  stage: build
  script:
    - echo a
"#,
        )?;
        fs::write(
            configs_dir.join("b.yml"),
            r#"
job-b:
  stage: build
  script:
    - echo b
"#,
        )?;

        let ci_dir = dir.path().join("ci");
        fs::create_dir_all(&ci_dir)?;
        let main_path = ci_dir.join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build
include:
  - local: configs/*.yml

main-job:
  stage: build
  script:
    - echo main
"#,
        )?;

        let pipeline = PipelineGraph::from_path(&main_path)?;
        assert!(pipeline.graph.node_weights().any(|job| job.name == "job-a"));
        assert!(pipeline.graph.node_weights().any(|job| job.name == "job-b"));
        assert!(
            pipeline
                .graph
                .node_weights()
                .any(|job| job.name == "main-job")
        );
        Ok(())
    }

    #[test]
    fn includes_local_paths_expand_environment_variables() -> Result<()> {
        let dir = tempdir()?;
        git2::Repository::init(dir.path())?;

        fs::write(
            dir.path().join("fragment.yml"),
            r#"
fragment-job:
  stage: build
  script:
    - echo fragment
"#,
        )?;

        let ci_dir = dir.path().join("ci");
        fs::create_dir_all(&ci_dir)?;
        let main_path = ci_dir.join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build
include:
  - local: $INCLUDE_FILE

main-job:
  stage: build
  script:
    - echo main
"#,
        )?;

        let pipeline = PipelineGraph::from_path_with_env(
            &main_path,
            HashMap::from([("INCLUDE_FILE".to_string(), "fragment.yml".to_string())]),
            None,
        )?;

        assert!(
            pipeline
                .graph
                .node_weights()
                .any(|job| job.name == "fragment-job")
        );
        assert!(
            pipeline
                .graph
                .node_weights()
                .any(|job| job.name == "main-job")
        );
        Ok(())
    }

    #[test]
    fn includes_local_files_list_from_repo_root() -> Result<()> {
        let dir = tempdir()?;
        git2::Repository::init(dir.path())?;

        let parts_dir = dir.path().join("parts");
        fs::create_dir_all(&parts_dir)?;
        fs::write(
            parts_dir.join("a.yml"),
            r#"
job-a:
  stage: build
  script:
    - echo a
"#,
        )?;
        fs::write(
            parts_dir.join("b.yml"),
            r#"
job-b:
  stage: build
  script:
    - echo b
"#,
        )?;

        let main_path = dir.path().join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build
include:
  files:
    - /parts/a.yml
    - /parts/b.yml

main-job:
  stage: build
  script:
    - echo main
"#,
        )?;

        let pipeline = PipelineGraph::from_path(&main_path)?;
        assert!(pipeline.graph.node_weights().any(|job| job.name == "job-a"));
        assert!(pipeline.graph.node_weights().any(|job| job.name == "job-b"));
        assert!(
            pipeline
                .graph
                .node_weights()
                .any(|job| job.name == "main-job")
        );
        Ok(())
    }

    #[test]
    fn includes_local_non_yaml_file_errors() {
        let dir = tempdir().expect("tempdir");
        git2::Repository::init(dir.path()).expect("init repo");

        fs::write(dir.path().join("fragment.txt"), "not yaml").expect("write fragment");

        let main_path = dir.path().join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build
include:
  - local: /fragment.txt

main-job:
  stage: build
  script:
    - echo main
"#,
        )
        .expect("write main");

        let err = PipelineGraph::from_path(&main_path).expect_err("non-yaml include must error");
        assert!(
            err.to_string()
                .contains("must reference a .yml or .yaml file")
        );
    }

    #[test]
    fn includes_file_alias_as_local_path() -> Result<()> {
        let dir = tempdir()?;
        git2::Repository::init(dir.path())?;

        fs::write(
            dir.path().join("fragment.yml"),
            r#"
fragment-job:
  stage: build
  script:
    - echo fragment
"#,
        )?;

        let main_path = dir.path().join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build
include:
  - file: /fragment.yml

main-job:
  stage: build
  script:
    - echo main
"#,
        )?;

        let pipeline = PipelineGraph::from_path(&main_path)?;
        assert!(
            pipeline
                .graph
                .node_weights()
                .any(|job| job.name == "fragment-job")
        );
        assert!(
            pipeline
                .graph
                .node_weights()
                .any(|job| job.name == "main-job")
        );
        Ok(())
    }

    #[test]
    fn retry_max_above_two_errors() {
        let dir = tempdir().expect("tempdir");
        let main_path = dir.path().join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build

build:
  stage: build
  retry: 3
  script:
    - echo hi
"#,
        )
        .expect("write main");

        let err = PipelineGraph::from_path(&main_path).expect_err("retry max must error");
        assert!(
            err.to_string()
                .contains("job 'build'.retry must be 0, 1, or 2")
        );
    }

    #[test]
    fn retry_exit_codes_parses_single_value() {
        let dir = tempdir().expect("tempdir");
        let main_path = dir.path().join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build

build:
  stage: build
  retry:
    max: 1
    exit_codes: 137
  script:
    - echo hi
"#,
        )
        .expect("write main");

        let pipeline = PipelineGraph::from_path(&main_path).expect("retry exit_codes must parse");
        let job = pipeline
            .graph
            .node_weights()
            .find(|job| job.name == "build")
            .expect("build job present");
        assert_eq!(job.retry.max, 1);
        assert_eq!(job.retry.exit_codes, vec![137]);
    }

    #[test]
    fn retry_exit_codes_rejects_negative_values() {
        let dir = tempdir().expect("tempdir");
        let main_path = dir.path().join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build

build:
  stage: build
  retry:
    max: 1
    exit_codes:
      - -1
  script:
    - echo hi
"#,
        )
        .expect("write main");

        let err = PipelineGraph::from_path(&main_path).expect_err("negative exit code must error");
        assert!(
            err.to_string()
                .contains("job 'build'.retry.exit_codes must contain non-negative integers")
        );
    }

    #[test]
    fn parses_artifacts_reports_dotenv() {
        let dir = tempdir().expect("tempdir");
        let main_path = dir.path().join("main.yml");
        fs::write(
            &main_path,
            r#"
stages:
  - build

build:
  stage: build
  script:
    - echo hi
  artifacts:
    reports:
      dotenv: tests-temp/dotenv/build.env
"#,
        )
        .expect("write main");

        let pipeline = PipelineGraph::from_path(&main_path).expect("pipeline parses");
        let job = pipeline
            .graph
            .node_weights()
            .find(|job| job.name == "build")
            .expect("build job present");

        assert_eq!(
            job.artifacts.report_dotenv.as_deref(),
            Some(std::path::Path::new("tests-temp/dotenv/build.env"))
        );
    }

    #[test]
    fn yaml_merge_keys_work_in_variables_mapping() {
        let pipeline = PipelineGraph::from_yaml_str(
            r#"
stages:
  - test

.base-vars: &base_vars
  SHARED_FLAG: from-anchor
  OVERRIDE_ME: old

merged-vars:
  stage: test
  variables:
    <<: *base_vars
    OVERRIDE_ME: new
  script:
    - echo ok
"#,
        )
        .expect("pipeline parses");

        let job = pipeline
            .graph
            .node_weights()
            .find(|job| job.name == "merged-vars")
            .expect("job present");

        assert_eq!(
            job.variables.get("SHARED_FLAG").map(String::as_str),
            Some("from-anchor")
        );
        assert_eq!(
            job.variables.get("OVERRIDE_ME").map(String::as_str),
            Some("new")
        );
    }

    #[test]
    fn yaml_merge_keys_work_in_job_mapping() {
        let pipeline = PipelineGraph::from_yaml_str(
            r#"
stages:
  - test

.job-template: &job_template
  stage: test
  script:
    - echo ok
  variables:
    INNER_FLAG: inner

merged-job:
  <<: *job_template
  variables:
    <<: { INNER_FLAG: inner, EXTRA_FLAG: extra }
"#,
        )
        .expect("pipeline parses");

        let job = pipeline
            .graph
            .node_weights()
            .find(|job| job.name == "merged-job")
            .expect("job present");

        assert_eq!(job.stage, "test");
        assert_eq!(job.commands, vec!["echo ok".to_string()]);
        assert_eq!(
            job.variables.get("INNER_FLAG").map(String::as_str),
            Some("inner")
        );
        assert_eq!(
            job.variables.get("EXTRA_FLAG").map(String::as_str),
            Some("extra")
        );
    }

    #[test]
    fn parses_image_docker_platform() {
        let pipeline = PipelineGraph::from_yaml_str(
            r#"
stages:
  - test

platform-job:
  stage: test
  image:
    name: docker.io/library/alpine:3.19
    docker:
      platform: linux/arm64/v8
  script:
    - echo ok
"#,
        )
        .expect("pipeline parses");

        let job = pipeline
            .graph
            .node_weights()
            .find(|job| job.name == "platform-job")
            .expect("job present");

        let image = job.image.as_ref().expect("image present");
        assert_eq!(image.name, "docker.io/library/alpine:3.19");
        assert_eq!(image.docker_platform.as_deref(), Some("linux/arm64/v8"));
    }

    #[test]
    fn parses_image_entrypoint_and_docker_user() {
        let pipeline = PipelineGraph::from_yaml_str(
            r#"
stages:
  - test

platform-job:
  stage: test
  image:
    name: docker.io/library/alpine:3.19
    entrypoint: [""]
    docker:
      user: 1000:1000
  script:
    - echo ok
"#,
        )
        .expect("pipeline parses");

        let job = pipeline
            .graph
            .node_weights()
            .find(|job| job.name == "platform-job")
            .expect("job present");

        let image = job.image.as_ref().expect("image present");
        assert_eq!(image.entrypoint, vec![""]);
        assert_eq!(image.docker_user.as_deref(), Some("1000:1000"));
    }

    #[test]
    fn parses_service_docker_platform_and_user() {
        let pipeline = PipelineGraph::from_yaml_str(
            r#"
stages:
  - test

service-job:
  stage: test
  image: docker.io/library/alpine:3.19
  services:
    - name: docker.io/library/redis:7.2
      alias: cache
      docker:
        platform: linux/arm64/v8
        user: 1000:1000
  script:
    - echo ok
"#,
        )
        .expect("pipeline parses");

        let job = pipeline
            .graph
            .node_weights()
            .find(|job| job.name == "service-job")
            .expect("job present");

        let service = job.services.first().expect("service present");
        assert_eq!(service.image, "docker.io/library/redis:7.2");
        assert_eq!(service.aliases, vec!["cache"]);
        assert_eq!(service.docker_platform.as_deref(), Some("linux/arm64/v8"));
        assert_eq!(service.docker_user.as_deref(), Some("1000:1000"));
    }

    #[test]
    fn inherit_default_controls_modeled_default_keywords() {
        let pipeline = PipelineGraph::from_yaml_str(
            r#"
stages:
  - test

default:
  image: docker.io/library/alpine:3.19
  before_script:
    - echo before
  after_script:
    - echo after
  cache:
    key: default-cache
    paths:
      - tests-temp/default-cache/
  services:
    - docker.io/library/redis:7.2
  timeout: 10m
  retry: 2
  interruptible: true

inherit-none:
  stage: test
  inherit:
    default: false
  image: docker.io/library/alpine:3.19
  script:
    - echo none

inherit-some:
  stage: test
  inherit:
    default: [image, retry, interruptible]
  script:
    - echo some
"#,
        )
        .expect("pipeline parses");

        let none = pipeline
            .graph
            .node_weights()
            .find(|job| job.name == "inherit-none")
            .expect("job present");
        assert!(!none.inherit_default_image);
        assert!(!none.inherit_default_before_script);
        assert!(!none.inherit_default_after_script);
        assert!(!none.inherit_default_cache);
        assert!(!none.inherit_default_services);
        assert!(!none.inherit_default_timeout);
        assert!(!none.inherit_default_retry);
        assert!(!none.inherit_default_interruptible);
        assert!(none.cache.is_empty());
        assert!(none.services.is_empty());
        assert_eq!(none.timeout, None);
        assert_eq!(none.retry.max, 0);
        assert!(!none.interruptible);

        let some = pipeline
            .graph
            .node_weights()
            .find(|job| job.name == "inherit-some")
            .expect("job present");
        assert!(some.inherit_default_image);
        assert!(!some.inherit_default_before_script);
        assert!(!some.inherit_default_after_script);
        assert!(!some.inherit_default_cache);
        assert!(!some.inherit_default_services);
        assert!(!some.inherit_default_timeout);
        assert!(some.inherit_default_retry);
        assert!(some.inherit_default_interruptible);
        assert_eq!(
            some.image.as_ref().map(|image| image.name.as_str()),
            Some("docker.io/library/alpine:3.19")
        );
        assert!(some.cache.is_empty());
        assert!(some.services.is_empty());
        assert_eq!(some.timeout, None);
        assert_eq!(some.retry.max, 2);
        assert!(some.interruptible);
    }

    #[test]
    fn include_cycle_errors() -> Result<()> {
        let dir = tempdir()?;
        let a_path = dir.path().join("a.yml");
        let b_path = dir.path().join("b.yml");
        fs::write(
            &a_path,
            format!(
                "
include:
  - local: {}
job-a:
  stage: build
  script: echo a
",
                b_path
                    .file_name()
                    .ok_or_else(|| anyhow!("missing b.yml filename"))?
                    .to_string_lossy()
            ),
        )?;
        fs::write(
            &b_path,
            format!(
                "
include:
  - local: {}
job-b:
  stage: build
  script: echo b
",
                a_path
                    .file_name()
                    .ok_or_else(|| anyhow!("missing a.yml filename"))?
                    .to_string_lossy()
            ),
        )?;

        let err = PipelineGraph::from_path(&a_path).expect_err("cycle must error");
        assert!(err.to_string().contains("include cycle"));
        Ok(())
    }

    #[test]
    fn records_needs_dependencies() {
        let yaml = r#"
stages:
  - build
  - deploy

build-job:
  stage: build
  script:
    - echo build

deploy-job:
  stage: deploy
  needs:
    - build-job
  script:
    - echo deploy
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let build_idx = find_job(&pipeline, "build-job");
        let deploy_idx = find_job(&pipeline, "deploy-job");

        assert_eq!(
            pipeline.graph[deploy_idx].needs[0].job,
            "build-job".to_string()
        );
        assert!(pipeline.graph[deploy_idx].needs[0].needs_artifacts);
        assert!(pipeline.graph.contains_edge(build_idx, deploy_idx));
    }

    #[test]
    fn parses_needs_without_artifacts() {
        let yaml = r#"
stages:
  - build
  - test

build-job:
  stage: build
  script:
    - echo build

test-job:
  stage: test
  needs:
    - job: build-job
      artifacts: false
  script:
    - echo test
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let test_idx = find_job(&pipeline, "test-job");
        assert_eq!(pipeline.graph[test_idx].needs.len(), 1);
        let need = &pipeline.graph[test_idx].needs[0];
        assert_eq!(need.job, "build-job");
        assert!(!need.needs_artifacts);
        assert!(!need.optional);
    }

    #[test]
    fn parses_optional_needs() {
        let yaml = r#"
stages:
  - build
  - test

build-job:
  stage: build
  script:
    - echo build

maybe-job:
  stage: build
  script:
    - echo maybe

test-job:
  stage: test
  needs:
    - build-job
    - job: maybe-job
      optional: true
  script:
    - echo test
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let test_idx = find_job(&pipeline, "test-job");
        assert_eq!(pipeline.graph[test_idx].needs.len(), 2);
        let need0 = &pipeline.graph[test_idx].needs[0];
        assert_eq!(need0.job, "build-job");
        assert!(!need0.optional);
        let need1 = &pipeline.graph[test_idx].needs[1];
        assert_eq!(need1.job, "maybe-job");
        assert!(need1.optional);
    }

    #[test]
    fn parses_artifacts_paths() {
        let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  artifacts:
    paths:
      - vendor/
      - output/report.txt
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let build_idx = find_job(&pipeline, "build-job");
        let job = &pipeline.graph[build_idx];
        assert_eq!(job.artifacts.paths.len(), 2);
        assert_eq!(job.artifacts.paths[0], PathBuf::from("vendor"));
        assert_eq!(job.artifacts.paths[1], PathBuf::from("output/report.txt"));
        assert_eq!(job.artifacts.when, ArtifactWhen::OnSuccess);
    }

    #[test]
    fn parses_artifacts_when() {
        let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  artifacts:
    when: always
    paths:
      - output/report.txt
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let build_idx = find_job(&pipeline, "build-job");
        let job = &pipeline.graph[build_idx];
        assert_eq!(job.artifacts.when, ArtifactWhen::Always);
    }

    #[test]
    fn parses_artifacts_exclude() {
        let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  artifacts:
    paths:
      - tests-temp/output/
    exclude:
      - tests-temp/output/**/*.log
      - tests-temp/output/ignore.txt
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let build_idx = find_job(&pipeline, "build-job");
        let job = &pipeline.graph[build_idx];
        assert_eq!(
            job.artifacts.exclude,
            vec![
                "tests-temp/output/**/*.log".to_string(),
                "tests-temp/output/ignore.txt".to_string()
            ]
        );
    }

    #[test]
    fn parses_cache_fallback_keys() {
        let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  cache:
    key: cache-$CI_COMMIT_REF_SLUG
    fallback_keys:
      - cache-$CI_DEFAULT_BRANCH
      - cache-default
    paths:
      - vendor/
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let build_idx = find_job(&pipeline, "build-job");
        let job = &pipeline.graph[build_idx];
        assert_eq!(
            job.cache[0].fallback_keys,
            vec![
                "cache-$CI_DEFAULT_BRANCH".to_string(),
                "cache-default".to_string()
            ]
        );
    }

    #[test]
    fn parses_cache_key_files_mapping() {
        let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  cache:
    key:
      files:
        - Cargo.lock
      prefix: $CI_JOB_NAME
    paths:
      - vendor/
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let build_idx = find_job(&pipeline, "build-job");
        let job = &pipeline.graph[build_idx];
        assert_eq!(
            job.cache[0].key,
            CacheKey::Files {
                files: vec![PathBuf::from("Cargo.lock")],
                prefix: Some("$CI_JOB_NAME".to_string()),
            }
        );
    }

    #[test]
    fn errors_when_cache_key_files_has_more_than_two_paths() {
        let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  cache:
    key:
      files:
        - Cargo.lock
        - package-lock.json
        - yarn.lock
    paths:
      - vendor/
"#;

        let err = PipelineGraph::from_yaml_str(yaml).expect_err("pipeline should fail");
        assert!(
            err.to_string()
                .contains("cache key map supports at most two files"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn parses_artifacts_untracked() {
        let yaml = r#"
stages:
  - build

build-job:
  stage: build
  script:
    - echo build
  artifacts:
    untracked: true
    exclude:
      - tests-temp/output/**/*.log
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let build_idx = find_job(&pipeline, "build-job");
        let job = &pipeline.graph[build_idx];
        assert!(job.artifacts.untracked);
        assert_eq!(
            job.artifacts.exclude,
            vec!["tests-temp/output/**/*.log".to_string()]
        );
    }

    #[test]
    fn parses_pipeline_and_job_images() {
        let yaml = r#"
image: rust:latest
stages:
  - build
  - test

build-job:
  stage: build
  image: rust:slim
  script:
    - echo build

test-job:
  stage: test
  script:
    - echo test
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        assert_eq!(
            pipeline
                .defaults
                .image
                .as_ref()
                .map(|image| image.name.as_str()),
            Some("rust:latest")
        );
        let build_idx = find_job(&pipeline, "build-job");
        assert_eq!(
            pipeline.graph[build_idx]
                .image
                .as_ref()
                .map(|image| image.name.as_str()),
            Some("rust:slim")
        );
        let test_idx = find_job(&pipeline, "test-job");
        assert_eq!(
            pipeline.graph[test_idx]
                .image
                .as_ref()
                .map(|image| image.name.as_str()),
            Some("rust:latest")
        );
    }

    #[test]
    fn ignores_default_section_as_job() {
        let yaml = r#"
stages:
  - build

default:
  image: alpine:latest
  before_script:
    - echo before
  after_script:
    - echo after
  variables:
    GLOBAL_DEFAULT: foo

build-job:
  stage: build
  script:
    - echo build
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        assert_eq!(pipeline.stages.len(), 1);
        assert_eq!(pipeline.stages[0].jobs.len(), 1);
        assert_eq!(
            pipeline.defaults.before_script,
            vec!["echo before".to_string()]
        );
        assert_eq!(
            pipeline.defaults.after_script,
            vec!["echo after".to_string()]
        );
        assert_eq!(
            pipeline
                .defaults
                .variables
                .get("GLOBAL_DEFAULT")
                .map(String::as_str),
            Some("foo")
        );
        let job_idx = pipeline.stages[0].jobs[0];
        assert_eq!(pipeline.graph[job_idx].name, "build-job");
    }

    #[test]
    fn parses_global_hooks() {
        let yaml = r#"
stages:
  - build

default:
  before_script:
    - echo before-one
    - echo before-two
  after_script: echo after

build-job:
  stage: build
  script: echo body
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        assert_eq!(
            pipeline.defaults.before_script,
            vec!["echo before-one".to_string(), "echo before-two".to_string()]
        );
        assert_eq!(
            pipeline.defaults.after_script,
            vec!["echo after".to_string()]
        );
    }

    #[test]
    fn parses_variable_scopes() {
        let yaml = r#"
variables:
  GLOBAL_VAR: foo

default:
  variables:
    DEFAULT_VAR: bar

stages:
  - build

build-job:
  stage: build
  variables:
    JOB_VAR: baz
  script:
    - echo job
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        assert_eq!(
            pipeline
                .defaults
                .variables
                .get("GLOBAL_VAR")
                .map(String::as_str),
            Some("foo")
        );
        assert_eq!(
            pipeline
                .defaults
                .variables
                .get("DEFAULT_VAR")
                .map(String::as_str),
            Some("bar")
        );
        let job_idx = find_job(&pipeline, "build-job");
        assert_eq!(
            pipeline.graph[job_idx]
                .variables
                .get("JOB_VAR")
                .map(String::as_str),
            Some("baz")
        );
    }

    #[test]
    fn ignores_non_job_sections_without_scripts() {
        let yaml = r#"
stages:
  - build

workflow:
  rules:
    - if: $CI_PIPELINE_SOURCE == "push"

build-job:
  stage: build
  script:
    - echo build
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        assert_eq!(pipeline.stages.len(), 1);
        assert_eq!(pipeline.stages[0].jobs.len(), 1);
        let job_idx = pipeline.stages[0].jobs[0];
        assert_eq!(pipeline.graph[job_idx].name, "build-job");
    }

    #[test]
    fn errors_when_job_missing_script() {
        let yaml = r#"
stages:
  - build

broken-job:
  stage: build
"#;

        let err = PipelineGraph::from_yaml_str(yaml).expect_err("missing script should error");
        assert!(err.to_string().contains("must define a script"));
    }

    #[test]
    fn ignores_hidden_jobs_starting_with_dot() {
        let yaml = r#"
stages:
  - build

.template:
  script:
    - echo template

build-job:
  stage: build
  script:
    - echo build
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        assert_eq!(pipeline.stages[0].jobs.len(), 1);
        let job_idx = pipeline.stages[0].jobs[0];
        assert_eq!(pipeline.graph[job_idx].name, "build-job");
    }

    #[test]
    fn job_can_extend_hidden_template() {
        let yaml = r#"
stages:
  - build

.base-template:
  stage: build
  script:
    - echo from template
  artifacts:
    paths:
      - template.txt

child-job:
  extends: .base-template
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let job_idx = find_job(&pipeline, "child-job");
        let job = &pipeline.graph[job_idx];
        assert_eq!(job.stage, "build");
        assert_eq!(job.commands, vec!["echo from template"]);
        assert_eq!(job.artifacts.paths, vec![PathBuf::from("template.txt")]);
    }

    #[test]
    fn job_merges_multiple_extends_in_order() {
        let yaml = r#"
stages:
  - test

.lint-template:
  script:
    - echo lint
  artifacts:
    paths:
      - lint.txt

.test-template:
  stage: test
  script:
    - echo tests
  artifacts:
    paths:
      - tests.txt

combined:
  extends:
    - .lint-template
    - .test-template
"#;

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        let job_idx = find_job(&pipeline, "combined");
        let job = &pipeline.graph[job_idx];
        assert_eq!(job.commands, vec!["echo tests"]);
        assert_eq!(job.artifacts.paths, vec![PathBuf::from("tests.txt")]);
        assert_eq!(job.stage, "test");
    }

    #[test]
    fn errors_on_extends_cycle() {
        let yaml = r#"
stages:
  - build

.a:
  extends: .b
  script:
    - echo a

.b:
  extends: .a
  script:
    - echo b

job:
  extends: .a
"#;

        let err = PipelineGraph::from_yaml_str(yaml).expect_err("cycle must error");
        assert!(err.to_string().contains("cyclical extends"));
    }

    #[test]
    fn errors_on_unknown_extended_template() {
        let yaml = r#"
stages:
  - build

job:
  stage: build
  extends: .missing
"#;

        let err = PipelineGraph::from_yaml_str(yaml).expect_err("unknown template must error");
        assert!(err.to_string().contains("unknown job/template '.missing'"));
    }

    fn find_job(graph: &PipelineGraph, name: &str) -> NodeIndex {
        graph
            .graph
            .node_indices()
            .find(|&idx| graph.graph[idx].name == name)
            .expect("job must exist")
    }
}
#[derive(Debug, Deserialize, Default)]
struct RawInherit {
    #[serde(default)]
    default: Option<RawInheritDefault>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawInheritDefault {
    Bool(bool),
    List(Vec<String>),
}
