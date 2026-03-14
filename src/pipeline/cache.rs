use crate::gitlab::CacheConfig;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct CacheManager {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CacheMountSpec {
    pub host: PathBuf,
    pub relative: PathBuf,
    pub read_only: bool,
}

impl CacheManager {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn mount_specs(
        &self,
        caches: &[CacheConfig],
        env: &HashMap<String, String>,
    ) -> Result<Vec<CacheMountSpec>> {
        if caches.is_empty() {
            return Ok(Vec::new());
        }

        let mut specs = Vec::new();
        for cache in caches {
            let key = render_cache_key(&cache.key, env);
            let entry_root = self.entry_root(&key);
            fs::create_dir_all(&entry_root).with_context(|| {
                format!("failed to prepare cache root {}", entry_root.display())
            })?;

            for relative in &cache.paths {
                let rel = cache_relative_path(relative);
                let host = entry_root.join(&rel);
                if !cache.policy.allows_pull() && host.exists() {
                    fs::remove_dir_all(&host).with_context(|| {
                        format!("failed to clear cache path {}", host.display())
                    })?;
                }
                fs::create_dir_all(&host)
                    .with_context(|| format!("failed to prepare cache path {}", host.display()))?;
                specs.push(CacheMountSpec {
                    host,
                    relative: relative.clone(),
                    read_only: !cache.policy.allows_push(),
                });
            }
        }

        Ok(specs)
    }

    fn entry_root(&self, key: &str) -> PathBuf {
        self.root.join(cache_dir_name(key))
    }
}

fn render_cache_key(template: &str, env: &HashMap<String, String>) -> String {
    expand_variables(template, env)
}

fn cache_dir_name(key: &str) -> String {
    let mut slug = String::new();
    for ch in key.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | '.') {
            slug.push('-');
        }
    }
    if slug.is_empty() {
        slug.push_str("cache");
    }
    let digest = Sha256::digest(key.as_bytes());
    let suffix = format!("{:x}", digest);
    let short = &suffix[..12];
    format!("{slug}-{short}")
}

fn cache_relative_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut rel = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => continue,
            Component::ParentDir => continue,
            Component::Prefix(prefix) => rel.push(prefix.as_os_str()),
            Component::Normal(seg) => rel.push(seg),
        }
    }

    if rel.as_os_str().is_empty() {
        rel.push("cache");
    }
    rel
}

fn expand_variables(template: &str, env: &HashMap<String, String>) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('$') => {
                out.push('$');
                chars.next();
            }
            Some('{') => {
                chars.next();
                let mut name = String::new();
                for next in chars.by_ref() {
                    if next == '}' {
                        break;
                    }
                    name.push(next);
                }
                if let Some(value) = env.get(&name) {
                    out.push_str(value);
                }
            }
            Some(c) if is_var_char(c) => {
                let mut name = String::new();
                name.push(c);
                chars.next();
                while let Some(&next) = chars.peek() {
                    if is_var_char(next) {
                        name.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if let Some(value) = env.get(&name) {
                    out.push_str(value);
                }
            }
            _ => {
                out.push('$');
            }
        }
    }
    out
}

fn is_var_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}
