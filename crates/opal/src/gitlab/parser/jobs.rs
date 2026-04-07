use anyhow::{Context, Result, anyhow, bail};
use globset::Glob;
use petgraph::graph::{DiGraph, NodeIndex};
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::path::PathBuf;
use std::time::Duration;
use tracing::warn;

use super::super::{
    graph::{
        ArtifactConfig, ArtifactWhen, CacheConfig, CacheKey, CachePolicy, DependencySource,
        EnvironmentAction, EnvironmentConfig, ExternalDependency, ImageConfig, Job, JobDependency,
        ParallelConfig, ParallelMatrixEntry, ParallelVariable, PipelineDefaults, PipelineFilters,
        PipelineGraph, RetryPolicy, ServiceConfig, StageGroup, WorkflowConfig,
    },
    rules::JobRule,
};
use super::merge_mappings;

pub(super) fn build_pipeline(root: Mapping) -> Result<PipelineGraph> {
    let mut stage_names: Vec<String> = Vec::new();
    let mut defaults = PipelineDefaults::default();
    let mut workflow: Option<WorkflowConfig> = None;
    let mut filters = PipelineFilters::default();

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
            Value::String(_) => {}
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
                    docker_user: docker_cfg.as_ref().and_then(|cfg| cfg.user.clone()),
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

fn build_graph(
    defaults: PipelineDefaults,
    workflow: Option<WorkflowConfig>,
    filters: PipelineFilters,
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

    for (job_name, job_idx, needs) in pending_needs {
        for dependency in needs {
            if !matches!(dependency.source, DependencySource::Local) {
                continue;
            }
            let Some(dependency_idx) = name_to_index.get(&dependency.job).copied() else {
                if dependency.optional {
                    continue;
                }
                return Err(anyhow!(
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
