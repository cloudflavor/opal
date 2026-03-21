use crate::model::{CacheKeySpec, CachePolicySpec, CacheSpec};
use crate::naming::job_name_slug;
use anyhow::{Context, Result};
use globset::Glob;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

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

#[derive(Debug, Clone)]
pub struct CacheEntryInfo {
    pub key: String,
    pub fallback_keys: Vec<String>,
    pub policy: CachePolicySpec,
    pub host: PathBuf,
    pub paths: Vec<PathBuf>,
}

impl CacheManager {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn mount_specs(
        &self,
        job_name: &str,
        staging_root: &Path,
        caches: &[CacheSpec],
        workspace: &Path,
        env: &HashMap<String, String>,
    ) -> Result<Vec<CacheMountSpec>> {
        if caches.is_empty() {
            return Ok(Vec::new());
        }

        let mut specs = Vec::new();
        for cache in caches {
            let key = resolve_cache_key(&cache.key, env, workspace);
            let fallback_keys: Vec<String> = cache
                .fallback_keys
                .iter()
                .map(|fallback| render_cache_key(fallback, env))
                .collect();
            let entry_root = self.entry_root(&key);
            fs::create_dir_all(&entry_root).with_context(|| {
                format!("failed to prepare cache root {}", entry_root.display())
            })?;
            for fallback in &fallback_keys {
                let fallback_root = self.entry_root(fallback);
                fs::create_dir_all(&fallback_root).with_context(|| {
                    format!("failed to prepare cache root {}", fallback_root.display())
                })?;
            }

            for relative in &cache.paths {
                let rel = cache_relative_path(relative);
                let entry_path = entry_root.join(&rel);
                let fallback_entry_paths: Vec<PathBuf> = fallback_keys
                    .iter()
                    .map(|fallback| self.entry_root(fallback).join(&rel))
                    .collect();
                let host = prepare_cache_mount(
                    cache.policy,
                    job_name,
                    staging_root,
                    &key,
                    &rel,
                    &entry_path,
                    &fallback_entry_paths,
                )?;
                specs.push(CacheMountSpec {
                    host,
                    relative: relative.clone(),
                    read_only: false,
                });
            }
        }

        Ok(specs)
    }

    pub fn describe_entries(
        &self,
        caches: &[CacheSpec],
        workspace: &Path,
        env: &HashMap<String, String>,
    ) -> Vec<CacheEntryInfo> {
        caches
            .iter()
            .map(|cache| {
                let key = resolve_cache_key(&cache.key, env, workspace);
                let host = self.entry_root(&key);
                CacheEntryInfo {
                    key,
                    fallback_keys: cache
                        .fallback_keys
                        .iter()
                        .map(|fallback| render_cache_key(fallback, env))
                        .collect(),
                    policy: cache.policy,
                    host,
                    paths: cache.paths.clone(),
                }
            })
            .collect()
    }

    fn entry_root(&self, key: &str) -> PathBuf {
        self.root.join(cache_dir_name(key))
    }
}

fn prepare_cache_mount(
    policy: CachePolicySpec,
    job_name: &str,
    staging_root: &Path,
    key: &str,
    relative: &Path,
    entry_path: &Path,
    fallback_entry_paths: &[PathBuf],
) -> Result<PathBuf> {
    match policy {
        CachePolicySpec::Pull => {
            let staged = staged_cache_path(staging_root, job_name, key, relative);
            reset_path(&staged)?;
            if let Some(source) = restore_source_path(entry_path, fallback_entry_paths) {
                copy_cache_path(source, &staged)?;
            } else {
                prepare_cache_path(&staged)?;
            }
            Ok(staged)
        }
        CachePolicySpec::Push => {
            reset_path(entry_path)?;
            prepare_cache_path(entry_path)?;
            Ok(entry_path.to_path_buf())
        }
        CachePolicySpec::PullPush => {
            if !entry_path.exists() {
                if let Some(source) = restore_source_path(entry_path, fallback_entry_paths) {
                    copy_cache_path(source, entry_path)?;
                } else {
                    prepare_cache_path(entry_path)?;
                }
            } else {
                prepare_cache_path(entry_path)?;
            }
            Ok(entry_path.to_path_buf())
        }
    }
}

fn restore_source_path<'a>(primary: &'a Path, fallbacks: &'a [PathBuf]) -> Option<&'a Path> {
    if primary.exists() {
        return Some(primary);
    }
    fallbacks
        .iter()
        .find(|candidate| candidate.exists())
        .map(PathBuf::as_path)
}

fn staged_cache_path(staging_root: &Path, job_name: &str, key: &str, relative: &Path) -> PathBuf {
    staging_root
        .join("cache-staging")
        .join(job_name_slug(job_name))
        .join(cache_dir_name(key))
        .join(relative)
}

fn prepare_cache_path(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to prepare cache path {}", path.display()))
}

fn reset_path(path: &Path) -> Result<()> {
    if path.exists() {
        remove_path(path)?;
    }
    Ok(())
}

fn copy_cache_path(src: &Path, dest: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(src).with_context(|| format!("failed to stat {}", src.display()))?;
    if metadata.is_dir() {
        fs::create_dir_all(dest).with_context(|| format!("failed to create {}", dest.display()))?;
        for entry in
            fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))?
        {
            let entry = entry?;
            let child_src = entry.path();
            let child_dest = dest.join(entry.file_name());
            copy_cache_path(&child_src, &child_dest)?;
        }
        return Ok(());
    }

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(src, dest)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dest.display()))?;
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))
    }
}

fn render_cache_key(template: &str, env: &HashMap<String, String>) -> String {
    expand_variables(template, env)
}

fn resolve_cache_key(
    cache_key: &CacheKeySpec,
    env: &HashMap<String, String>,
    workspace: &Path,
) -> String {
    match cache_key {
        CacheKeySpec::Literal(template) => render_cache_key(template, env),
        CacheKeySpec::Files { files, prefix } => {
            files_cache_key(files, prefix.as_deref(), env, workspace)
        }
    }
}

fn files_cache_key(
    files: &[PathBuf],
    prefix: Option<&str>,
    env: &HashMap<String, String>,
    workspace: &Path,
) -> String {
    let mut matched = Vec::new();
    for file in files {
        matched.extend(resolve_cache_key_file_entry(file, workspace));
    }
    matched.sort();
    matched.dedup();

    let suffix = if matched.is_empty() {
        "default".to_string()
    } else {
        let mut digest = Sha256::new();
        let mut had_input = false;
        for path in matched {
            if let Ok(bytes) = fs::read(&path) {
                digest.update(&bytes);
                had_input = true;
            }
        }
        if had_input {
            format!("{:x}", digest.finalize())
        } else {
            "default".to_string()
        }
    };

    if let Some(prefix) = prefix {
        let rendered = expand_variables(prefix, env);
        if !rendered.is_empty() {
            return format!("{rendered}-{suffix}");
        }
    }
    suffix
}

fn resolve_cache_key_file_entry(entry: &Path, workspace: &Path) -> Vec<PathBuf> {
    let pattern = entry.to_string_lossy();
    if has_glob_pattern(&pattern) {
        let Ok(glob) = Glob::new(&pattern) else {
            return Vec::new();
        };
        let matcher = glob.compile_matcher();
        let mut matches = Vec::new();
        for walk in WalkDir::new(workspace)
            .follow_links(false)
            .into_iter()
            .flatten()
        {
            if !walk.path().is_file() {
                continue;
            }
            let Ok(relative) = walk.path().strip_prefix(workspace) else {
                continue;
            };
            if matcher.is_match(relative) {
                matches.push(walk.path().to_path_buf());
            }
        }
        return matches;
    }
    let path = if entry.is_absolute() {
        entry.to_path_buf()
    } else {
        workspace.join(entry)
    };
    if path.is_file() {
        vec![path]
    } else {
        Vec::new()
    }
}

fn has_glob_pattern(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.contains('[') || value.contains('{')
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

#[cfg(test)]
mod tests {
    use super::{CacheManager, cache_dir_name};
    use crate::model::{CacheKeySpec, CachePolicySpec, CacheSpec};
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn pull_policy_stages_cache_into_job_local_copy() {
        let root = temp_path("cache-pull");
        let manager = CacheManager::new(root.join("cache-root"));
        let key = "branch-main";
        let entry = root
            .join("cache-root")
            .join(cache_dir_name(key))
            .join("tests-temp/cache-data");
        fs::create_dir_all(&entry).expect("create cache entry");
        fs::write(entry.join("seed.txt"), "seed").expect("write seed");

        let specs = manager
            .mount_specs(
                "test-job",
                &root.join("session"),
                &[cache("tests-temp/cache-data/", key, CachePolicySpec::Pull)],
                &root,
                &HashMap::new(),
            )
            .expect("mount specs");

        assert_eq!(specs.len(), 1);
        assert!(!specs[0].read_only);
        assert!(
            specs[0]
                .host
                .starts_with(root.join("session").join("cache-staging").join("test-job"))
        );
        assert!(specs[0].host.ends_with(Path::new("tests-temp/cache-data")));
        assert!(specs[0].host.join("seed.txt").exists());

        fs::write(specs[0].host.join("seed.txt"), "mutated").expect("mutate staged copy");
        assert_eq!(
            fs::read_to_string(entry.join("seed.txt")).expect("read original"),
            "seed"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn push_policy_restarts_from_empty_cache_path() {
        let root = temp_path("cache-push");
        let manager = CacheManager::new(root.join("cache-root"));
        let key = "branch-main";
        let entry = root
            .join("cache-root")
            .join(cache_dir_name(key))
            .join("tests-temp/cache-data");
        fs::create_dir_all(&entry).expect("create cache entry");
        fs::write(entry.join("old.txt"), "old").expect("write old");

        let specs = manager
            .mount_specs(
                "seed-job",
                &root.join("session"),
                &[cache("tests-temp/cache-data/", key, CachePolicySpec::Push)],
                &root,
                &HashMap::new(),
            )
            .expect("mount specs");

        assert_eq!(specs[0].host, entry);
        assert!(!specs[0].host.join("old.txt").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pull_push_policy_restores_from_fallback_key_into_primary() {
        let root = temp_path("cache-fallback");
        let manager = CacheManager::new(root.join("cache-root"));
        let primary_key = "branch-feature";
        let fallback_key = "branch-main";
        let fallback_entry = root
            .join("cache-root")
            .join(cache_dir_name(fallback_key))
            .join("tests-temp/cache-data");
        fs::create_dir_all(&fallback_entry).expect("create fallback entry");
        fs::write(fallback_entry.join("seed.txt"), "fallback").expect("write fallback");

        let specs = manager
            .mount_specs(
                "verify-job",
                &root.join("session"),
                &[cache_with_fallback(
                    "tests-temp/cache-data/",
                    primary_key,
                    &[fallback_key],
                    CachePolicySpec::PullPush,
                )],
                &root,
                &HashMap::new(),
            )
            .expect("mount specs");

        let primary_entry = root
            .join("cache-root")
            .join(cache_dir_name(primary_key))
            .join("tests-temp/cache-data");

        assert_eq!(specs[0].host, primary_entry);
        assert_eq!(
            fs::read_to_string(primary_entry.join("seed.txt")).expect("read restored"),
            "fallback"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn files_cache_key_uses_workspace_file_content_with_prefix() {
        let root = temp_path("cache-files-key");
        let manager = CacheManager::new(root.join("cache-root"));
        fs::create_dir_all(&root).expect("create root");
        fs::write(root.join("Cargo.lock"), "content-v1").expect("write lockfile");

        let entries = manager.describe_entries(
            &[CacheSpec {
                key: CacheKeySpec::Files {
                    files: vec![PathBuf::from("Cargo.lock")],
                    prefix: Some("$CI_JOB_NAME".to_string()),
                },
                fallback_keys: Vec::new(),
                paths: vec![PathBuf::from("target")],
                policy: CachePolicySpec::PullPush,
            }],
            &root,
            &HashMap::from([("CI_JOB_NAME".to_string(), "lint".to_string())]),
        );

        assert_eq!(entries.len(), 1);
        assert!(entries[0].key.starts_with("lint-"));
        assert_ne!(entries[0].key, "lint-default");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn files_cache_key_falls_back_to_default_when_files_missing() {
        let root = temp_path("cache-files-default");
        let manager = CacheManager::new(root.join("cache-root"));
        fs::create_dir_all(&root).expect("create root");

        let entries = manager.describe_entries(
            &[CacheSpec {
                key: CacheKeySpec::Files {
                    files: vec![PathBuf::from("missing.lock")],
                    prefix: Some("deps".to_string()),
                },
                fallback_keys: Vec::new(),
                paths: vec![PathBuf::from("target")],
                policy: CachePolicySpec::PullPush,
            }],
            &root,
            &HashMap::new(),
        );

        assert_eq!(entries[0].key, "deps-default");

        let _ = fs::remove_dir_all(root);
    }

    fn cache(path: &str, key: &str, policy: CachePolicySpec) -> CacheSpec {
        cache_with_fallback(path, key, &[], policy)
    }

    fn cache_with_fallback(
        path: &str,
        key: &str,
        fallback_keys: &[&str],
        policy: CachePolicySpec,
    ) -> CacheSpec {
        CacheSpec {
            key: CacheKeySpec::Literal(key.into()),
            fallback_keys: fallback_keys.iter().map(|key| (*key).to_string()).collect(),
            paths: vec![PathBuf::from(path)],
            policy,
        }
    }

    fn temp_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("opal-{prefix}-{nanos}"))
    }
}
