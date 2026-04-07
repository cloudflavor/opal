use anyhow::{Context, Result, anyhow, bail};
use serde_yaml::value::TaggedValue;
use serde_yaml::{Mapping, Value};
use std::convert::TryFrom;

pub(super) fn normalize_root(root: Mapping) -> Result<Mapping> {
    resolve_yaml_merge_keys(resolve_reference_tags(root)?)
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
            for (key, val) in map {
                let resolved_key = resolve_references(key, root, visiting)?;
                let resolved_val = resolve_references(val, root, visiting)?;
                resolved.insert(resolved_key, resolved_val);
            }
            Ok(Value::Mapping(resolved))
        }
        Value::Sequence(seq) => {
            let mut resolved = Vec::with_capacity(seq.len());
            for entry in seq {
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
    for entry in entries {
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
