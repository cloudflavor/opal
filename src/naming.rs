pub fn job_name_slug(name: &str) -> String {
    let mut slug = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else {
            match ch {
                ' ' | '-' | '_' => slug.push('-'),
                _ => continue,
            }
        }
    }

    if slug.is_empty() {
        slug.push_str("job");
    }

    slug
}

pub fn stage_name_slug(name: &str) -> String {
    job_name_slug(name)
}

pub fn escape_double_quotes(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

use crate::ExecutorConfig;
use sha2::{Digest, Sha256};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn generate_run_id(config: &ExecutorConfig) -> String {
    let pipeline_slug = config
        .pipeline
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(job_name_slug)
        .unwrap_or_else(|| "pipeline".to_string());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let mut hasher = Sha256::new();
    hasher.update(pipeline_slug.as_bytes());
    hasher.update(nanos.to_le_bytes());
    hasher.update(process::id().to_le_bytes());

    let digest = hasher.finalize();
    let suffix = format!("{:x}", digest);
    let short = &suffix[..8];
    format!("{pipeline_slug}-{short}")
}
