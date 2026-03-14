use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use humantime;
use petgraph::graph::{DiGraph, NodeIndex};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use tracing::warn;

use super::graph::{
    CacheConfig, CachePolicy, DependencySource, ExternalDependency, Job, JobDependency,
    PipelineDefaults, PipelineGraph, RetryPolicy, ServiceConfig, StageGroup, WorkflowConfig,
};
use super::rules::JobRule;

impl PipelineGraph {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let canonical =
            fs::canonicalize(path).with_context(|| format!("failed to resolve {:?}", path))?;
        let mut stack = Vec::new();
        let root = load_pipeline_file(&canonical, &mut stack)?;
        Self::from_mapping(root)
    }

    pub fn from_yaml_str(contents: &str) -> Result<Self> {
        let root: Mapping = serde_yaml::from_str(contents)?;
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

fn load_pipeline_file(path: &Path, stack: &mut Vec<PathBuf>) -> Result<Mapping> {
    if stack.iter().any(|p| p == path) {
        bail!("include cycle detected involving {:?}", path);
    }
    stack.push(path.to_path_buf());

    let content = fs::read_to_string(path).with_context(|| format!("failed to read {:?}", path))?;
    let mut root: Mapping = serde_yaml::from_str(&content)?;
    let include_key = Value::String("include".to_string());
    let mut combined = Mapping::new();

    if let Some(include_value) = root.remove(&include_key) {
        let includes = parse_include_entries(include_value)?;
        for include in includes {
            let resolved = if include.is_absolute() {
                include
            } else {
                path.parent().unwrap_or(Path::new(".")).join(include)
            };
            let canonical = fs::canonicalize(&resolved)
                .with_context(|| format!("failed to resolve include {:?}", resolved))?;
            let included = load_pipeline_file(&canonical, stack)?;
            combined = merge_mappings(combined, included);
        }
    }

    combined = merge_mappings(combined, root);
    stack.pop();
    Ok(combined)
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
                defaults.retry = raw.into_policy(&RetryPolicy::default());
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

fn parse_image(value: Value) -> Result<String> {
    match value {
        Value::String(name) => Ok(name),
        Value::Mapping(mut map) => {
            if let Some(val) = map.remove(Value::String("name".to_string())) {
                extract_string(val, "image name")
            } else {
                bail!("image mapping must include 'name'")
            }
        }
        other => bail!("image must be a string or mapping, got {other:?}"),
    }
}

fn extract_string(value: Value, what: &str) -> Result<String> {
    match value {
        Value::String(text) => Ok(text),
        other => bail!("{what} must be a string, got {other:?}"),
    }
}

type ParsedJobSpec = (
    RawJob,
    Option<String>,
    HashMap<String, String>,
    Vec<CacheConfig>,
    Vec<ServiceConfig>,
);

fn parse_job(value: Value) -> Result<ParsedJobSpec> {
    match value {
        Value::Mapping(mut map) => {
            let image_value = map.remove(Value::String("image".to_string()));
            let variables_value = map.remove(Value::String("variables".to_string()));
            let cache_value = map.remove(Value::String("cache".to_string()));
            let services_value = map.remove(Value::String("services".to_string()));
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
            Ok((job_spec, image, variables, cache, services))
        }
        other => bail!("job definition must be a mapping, got {other:?}"),
    }
}

fn parse_string_list(value: Value, field: &str) -> Result<Vec<String>> {
    match value {
        Value::Sequence(entries) => entries
            .into_iter()
            .map(|val| match val {
                Value::String(text) => Ok(text),
                other => bail!("{field} entries must be strings, got {other:?}"),
            })
            .collect(),
        Value::String(text) => Ok(vec![text]),
        Value::Null => Ok(Vec::new()),
        other => bail!("{field} must be a string or sequence, got {other:?}"),
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

fn parse_cache_entry(value: Value) -> Result<CacheConfig> {
    let raw: CacheEntryRaw = match value {
        Value::Mapping(_) => serde_yaml::from_value(value)?,
        other => bail!("cache entry must be a mapping, got {other:?}"),
    };
    let key = raw.key.unwrap_or_else(|| "default".to_string());
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
    Ok(CacheConfig { key, paths, policy })
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
        let value = extract_string(val, &format!("variable '{name}'"))?;
        vars.insert(name, value);
    }

    Ok(vars)
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
                alias: None,
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
        Value::Null => Ok(Vec::new()),
        other => bail!("{field} must be a string or list, got {other:?}"),
    }
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
        let (job_spec, job_image, job_variables, job_cache, job_services) =
            parse_job(Value::Mapping(merged_map))?;
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
        let artifacts = job_spec.artifacts.paths;
        let cache_entries = if job_cache.is_empty() {
            defaults.cache.clone()
        } else {
            job_cache
        };
        let services = if job_services.is_empty() {
            defaults.services.clone()
        } else {
            job_services
        };
        let timeout =
            parse_optional_timeout(&job_spec.timeout, &format!("job '{}'.timeout", job_name))?
                .or(defaults.timeout);
        let retry = job_spec
            .retry
            .map(|raw| raw.into_policy(&defaults.retry))
            .unwrap_or_else(|| defaults.retry.clone());
        let interruptible = job_spec.interruptible.unwrap_or(defaults.interruptible);
        let resource_group = job_spec.resource_group.clone();

        let node = graph.add_node(Job {
            name: job_name.clone(),
            stage: stage_name,
            commands,
            needs: needs.clone(),
            explicit_needs,
            dependencies: dependencies.clone(),
            before_script,
            after_script,
            rules: job_spec.rules.clone(),
            artifacts,
            cache: cache_entries,
            image: job_image,
            variables: job_variables,
            services,
            timeout,
            retry,
            interruptible,
            resource_group,
        });

        name_to_index.insert(job_name.clone(), node);
        pending_needs.push((job_name, node, needs));

        stages
            .get_mut(stage_index)
            .expect("stage index must exist")
            .jobs
            .push(node);
    }

    for (job_name, job_idx, needs) in pending_needs {
        for dependency in needs {
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

fn parse_include_entries(value: Value) -> Result<Vec<PathBuf>> {
    match value {
        Value::String(path) => Ok(vec![PathBuf::from(path)]),
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

fn parse_include_entry(value: Value) -> Result<Vec<PathBuf>> {
    match value {
        Value::String(path) => Ok(vec![PathBuf::from(path)]),
        Value::Mapping(map) => {
            let local_key = Value::String("local".to_string());
            let file_key = Value::String("file".to_string());
            let files_key = Value::String("files".to_string());
            if let Some(Value::String(local)) = map.get(&local_key) {
                Ok(vec![PathBuf::from(local)])
            } else if let Some(Value::String(file)) = map.get(&file_key) {
                Ok(vec![PathBuf::from(file)])
            } else if let Some(Value::Sequence(files)) = map.get(&files_key) {
                let mut paths = Vec::new();
                for entry in files {
                    match entry {
                        Value::String(path) => paths.push(PathBuf::from(path)),
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
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Script {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Deserialize, Default)]
struct RawArtifacts {
    #[serde(default)]
    paths: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawService {
    Simple(String),
    Detailed(RawServiceConfig),
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
        Ok(ServiceConfig {
            image,
            alias: self.alias,
            entrypoint: self.entrypoint.into_vec(),
            command: self.command.into_vec(),
            variables: self.variables,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawRetry {
    Simple(u32),
    Detailed(RawRetryConfig),
}

impl RawRetry {
    fn into_policy(self, base: &RetryPolicy) -> RetryPolicy {
        match self {
            RawRetry::Simple(max) => RetryPolicy {
                max,
                when: base.when.clone(),
            },
            RawRetry::Detailed(cfg) => {
                let mut policy = base.clone();
                if let Some(max) = cfg.max {
                    policy.max = max;
                }
                if !cfg.when.0.is_empty() {
                    policy.when = cfg.when.into_vec();
                }
                policy
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawRetryConfig {
    #[serde(default)]
    max: Option<u32>,
    #[serde(default)]
    when: StringList,
}

#[derive(Debug, Default)]
struct StringList(Vec<String>);

impl StringList {
    fn into_vec(self) -> Vec<String> {
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
        struct VisitorImpl;

        impl<'de> serde::de::Visitor<'de> for VisitorImpl {
            type Value = ServiceCommand;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("string or sequence of strings")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ServiceCommand(vec![v.to_string()]))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut items = Vec::new();
                while let Some(entry) = seq.next_element::<String>()? {
                    items.push(entry);
                }
                Ok(ServiceCommand(items))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ServiceCommand(Vec::new()))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ServiceCommand(Vec::new()))
            }
        }

        deserializer.deserialize_any(VisitorImpl)
    }
}

impl<'de> serde::Deserialize<'de> for StringList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        struct VisitorImpl;

        impl<'de> serde::de::Visitor<'de> for VisitorImpl {
            type Value = StringList;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("string or sequence of strings")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(StringList(vec![v.to_string()]))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut items = Vec::new();
                while let Some(entry) = seq.next_element::<String>()? {
                    items.push(entry);
                }
                Ok(StringList(items))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(StringList(Vec::new()))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(StringList(Vec::new()))
            }
        }

        deserializer.deserialize_any(VisitorImpl)
    }
}

#[derive(Debug, Deserialize, Default)]
struct CacheEntryRaw {
    key: Option<String>,
    #[serde(default)]
    paths: Vec<PathBuf>,
    #[serde(default)]
    policy: Option<String>,
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
}

impl Need {
    fn into_dependency(self, owner: &str) -> Option<JobDependency> {
        match self {
            Need::Name(job) => Some(JobDependency {
                job,
                needs_artifacts: true,
                optional: false,
                source: DependencySource::Local,
            }),
            Need::Config(cfg) => {
                let NeedConfig {
                    job,
                    artifacts,
                    optional,
                    project,
                    reference,
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
                    })
                } else {
                    Some(JobDependency {
                        job,
                        needs_artifacts: artifacts,
                        optional,
                        source: DependencySource::Local,
                    })
                }
            }
        }
    }
}

fn default_artifacts_true() -> bool {
    true
}

impl Default for Script {
    fn default() -> Self {
        Script::Multiple(Vec::new())
    }
}

impl Script {
    fn into_commands(self) -> Vec<String> {
        match self {
            Script::Single(line) => vec![line],
            Script::Multiple(lines) => lines,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parses_stage_and_job_order() {
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

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
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
    }

    #[test]
    fn includes_local_fragment() {
        let dir = tempdir().expect("tempdir");
        let fragment_path = dir.path().join("fragment.yml");
        fs::write(
            &fragment_path,
            r#"
fragment-job:
  stage: build
  script:
    - echo fragment
"#,
        )
        .expect("write fragment");

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
        )
        .expect("write main");

        let pipeline = PipelineGraph::from_path(&main_path).expect("pipeline parses");
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
    }

    #[test]
    fn include_cycle_errors() {
        let dir = tempdir().expect("tempdir");
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
                b_path.file_name().unwrap().to_string_lossy()
            ),
        )
        .expect("write a");
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
                a_path.file_name().unwrap().to_string_lossy()
            ),
        )
        .expect("write b");

        let err = PipelineGraph::from_path(&a_path).expect_err("cycle must error");
        assert!(err.to_string().contains("include cycle"));
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
        assert_eq!(job.artifacts.len(), 2);
        assert_eq!(job.artifacts[0], PathBuf::from("vendor"));
        assert_eq!(job.artifacts[1], PathBuf::from("output/report.txt"));
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
        assert_eq!(pipeline.defaults.image.as_deref(), Some("rust:latest"));
        let build_idx = find_job(&pipeline, "build-job");
        assert_eq!(
            pipeline.graph[build_idx].image.as_deref(),
            Some("rust:slim")
        );
        let test_idx = find_job(&pipeline, "test-job");
        assert!(pipeline.graph[test_idx].image.is_none());
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
        assert_eq!(job.artifacts, vec![PathBuf::from("template.txt")]);
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
        assert_eq!(job.artifacts, vec![PathBuf::from("tests.txt")]);
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
