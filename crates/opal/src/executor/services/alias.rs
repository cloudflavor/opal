use crate::model::ServiceSpec;
use anyhow::{Result, bail};
use std::collections::HashSet;

pub(super) struct ServiceAliasRegistry {
    claimed_aliases: HashSet<String>,
}

impl ServiceAliasRegistry {
    pub(super) fn new() -> Self {
        Self {
            claimed_aliases: HashSet::new(),
        }
    }

    pub(super) fn aliases_for_service(
        &mut self,
        idx: usize,
        service: &ServiceSpec,
    ) -> Result<Vec<String>> {
        let mut accepted = Vec::new();
        if service.aliases.is_empty() {
            for alias in default_service_aliases(&service.image) {
                if self.claimed_aliases.insert(alias.clone()) {
                    accepted.push(alias);
                }
            }
        } else {
            for candidate in &service.aliases {
                let alias = validate_service_alias(candidate)?;
                if self.claimed_aliases.insert(alias.clone()) {
                    accepted.push(alias);
                }
            }
        }

        if accepted.is_empty() {
            let fallback = validate_service_alias(&format!("svc-{idx}"))?;
            self.claimed_aliases.insert(fallback.clone());
            accepted.push(fallback);
        }

        Ok(accepted)
    }
}

fn validate_service_alias(alias: &str) -> Result<String> {
    let normalized = alias.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        bail!("service alias must not be empty");
    }
    if normalized.starts_with('-') || normalized.ends_with('-') {
        bail!("service alias '{}' must not start or end with '-'", alias);
    }
    if !normalized
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        bail!(
            "service alias '{}' contains unsupported characters; use lowercase letters, digits, or '-'",
            alias
        );
    }
    Ok(normalized)
}

fn default_service_aliases(image: &str) -> Vec<String> {
    let without_tag = image.split(':').next().unwrap_or(image);
    let primary = without_tag.replace('/', "__");
    let secondary = without_tag.replace('/', "-");
    let mut aliases = Vec::new();
    if !primary.is_empty() {
        aliases.push(primary);
    }
    if !secondary.is_empty() && !aliases.iter().any(|existing| existing == &secondary) {
        aliases.push(secondary);
    }
    if aliases.is_empty() {
        aliases.push("service".to_string());
    }
    aliases
}
