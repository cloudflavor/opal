use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use humantime;
use petgraph::graph::{DiGraph, NodeIndex};
use serde::Deserialize;
use serde_yaml::value::TaggedValue;
use serde_yaml::{Mapping, Value};
use tracing::warn;

use super::graph::{
    CacheConfig, CachePolicy, DependencySource, EnvironmentAction, EnvironmentConfig,
    ExternalDependency, Job, JobDependency, ParallelConfig, ParallelMatrixEntry, ParallelVariable,
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
        let root = resolve_reference_tags(root)?;
        Self::from_mapping(root)
    }

    pub fn from_yaml_str(contents: &str) -> Result<Self> {
        let root: Mapping = serde_yaml::from_str(contents)?;
        let root = resolve_reference_tags(root)?;
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
    let (key, value) = map.into_iter().next().expect("checked length");
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
        let inherit_flags = job_inherit_flags(&job_spec);
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
        let parallel = job_parallel;

        let environment = job_spec.environment.as_ref().map(|env| {
            let action = match env.action.as_deref() {
                Some("stop") => EnvironmentAction::Stop,
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
            inherit_default_before_script: inherit_flags.0,
            inherit_default_after_script: inherit_flags.1,
            parallel,
            only,
            except,
            tags: job_spec.tags.clone(),
            environment,
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

fn job_inherit_flags(job: &RawJob) -> (bool, bool) {
    let mut inherit_before = true;
    let mut inherit_after = true;
    if let Some(inherit) = &job.inherit
        && let Some(default) = &inherit.default
    {
        match default {
            RawInheritDefault::Bool(value) => {
                inherit_before = *value;
                inherit_after = *value;
            }
            RawInheritDefault::List(entries) => {
                inherit_before = entries.iter().any(|entry| entry == "before_script");
                inherit_after = entries.iter().any(|entry| entry == "after_script");
            }
        }
    }
    (inherit_before, inherit_after)
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
    fn resolves_reference_tags() {
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

        let pipeline = PipelineGraph::from_yaml_str(yaml).expect("pipeline parses");
        assert_eq!(pipeline.stages.len(), 1);
        let build_stage = &pipeline.stages[0];
        assert_eq!(build_stage.jobs.len(), 1);
        let job = &pipeline.graph[build_stage.jobs[0]];
        assert_eq!(job.commands, vec!["echo shared"]);
        assert_eq!(
            job.variables.get("COPIED").map(|value| value.as_str()),
            Some("shared-value")
        );
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
