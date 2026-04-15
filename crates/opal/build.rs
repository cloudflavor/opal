use git2::{DescribeFormatOptions, DescribeOptions, Repository, StatusOptions};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[path = "src/version_scheme.rs"]
mod version_scheme;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let workspace_root = manifest_dir.join("../..");
    let fallback_root = manifest_dir.join("embedded");
    let docs_src = preferred_required_dir(workspace_root.join("docs"), manifest_dir.join("docs"));
    let prompts_src = preferred_dir(
        workspace_root.join("prompts").join("ai"),
        fallback_root.join("prompts").join("ai"),
    );

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("out dir"));
    let docs_out = out_dir.join("embedded-docs");
    let prompts_out = out_dir.join("embedded-prompts-ai");
    let repository = open_repository(&workspace_root);

    copy_dir_contents(&docs_src, &docs_out).expect("copy embedded docs");
    copy_dir_contents(&prompts_src, &prompts_out).expect("copy embedded prompts");

    println!(
        "cargo:rustc-env=OPAL_EMBEDDED_DOCS_DIR={}",
        docs_out.display()
    );
    println!(
        "cargo:rustc-env=OPAL_EMBEDDED_PROMPTS_DIR={}",
        prompts_out.display()
    );
    println!(
        "cargo:rustc-env=OPAL_BUILD_VERSION={}",
        compute_build_version(repository.as_ref())
    );

    println!("cargo:rerun-if-changed={}", docs_src.display());
    println!("cargo:rerun-if-changed={}", prompts_src.display());
    println!(
        "cargo:rerun-if-changed={}",
        fallback_root.join("prompts").join("ai").display()
    );
    emit_git_rerun_hints(&workspace_root, repository.as_ref());
}

fn required_dir(path: PathBuf) -> PathBuf {
    assert!(
        path.is_dir(),
        "required embedded source directory is missing: {}",
        path.display()
    );
    path
}

fn preferred_required_dir(primary: PathBuf, fallback: PathBuf) -> PathBuf {
    if primary.is_dir() {
        primary
    } else {
        required_dir(fallback)
    }
}

fn preferred_dir(primary: PathBuf, fallback: PathBuf) -> PathBuf {
    if primary.is_dir() { primary } else { fallback }
}

fn copy_dir_contents(source: &Path, destination: &Path) -> io::Result<()> {
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if path.is_dir() {
            copy_dir_contents(&path, &target)?;
        } else {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

fn open_repository(workspace_root: &Path) -> Option<Repository> {
    Repository::open(workspace_root).ok()
}

fn compute_build_version(repository: Option<&Repository>) -> String {
    let package_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| String::from("0.0.0"));
    let dirty = repository.and_then(is_workspace_dirty).unwrap_or(false);

    if let Some(describe) = repository.and_then(release_describe)
        && let Some(version) = version_scheme::version_from_git_describe(&describe, dirty)
    {
        return version;
    }

    let explicit_tag = env::var("CI_COMMIT_TAG")
        .ok()
        .or_else(|| env::var("GIT_COMMIT_TAG").ok());
    if let Some(explicit_tag) = explicit_tag
        && let Some(tag) = version_scheme::parse_release_tag(explicit_tag.trim())
    {
        let mut version = tag.core();
        if dirty {
            version_scheme::append_dirty_metadata(&mut version);
        }
        return version;
    }

    let short_sha = repository.and_then(head_short_sha);
    version_scheme::fallback_version(&package_version, short_sha.as_deref(), dirty)
}

fn release_describe(repository: &Repository) -> Option<String> {
    let mut describe_options = DescribeOptions::new();
    describe_options
        .describe_tags()
        .pattern("v[0-9]*.[0-9]*.[0-9]*");

    let describe = repository.describe(&describe_options).ok()?;
    let mut format_options = DescribeFormatOptions::new();
    format_options
        .always_use_long_format(true)
        .abbreviated_size(7);
    describe.format(Some(&format_options)).ok()
}

fn is_workspace_dirty(repository: &Repository) -> Option<bool> {
    let mut options = StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repository.statuses(Some(&mut options)).ok()?;
    Some(!statuses.is_empty())
}

fn head_short_sha(repository: &Repository) -> Option<String> {
    let head_oid = repository.head().ok()?.target()?;
    let full = head_oid.to_string();
    Some(full.chars().take(7).collect())
}

fn emit_git_rerun_hints(workspace_root: &Path, repository: Option<&Repository>) {
    for candidate in [
        workspace_root.join(".git"),
        workspace_root.join(".git").join("HEAD"),
    ] {
        println!("cargo:rerun-if-changed={}", candidate.display());
    }

    if let Some(repository) = repository {
        let git_dir = repository.path().to_path_buf();
        for candidate in [
            git_dir.clone(),
            git_dir.join("HEAD"),
            git_dir.join("index"),
            git_dir.join("packed-refs"),
            git_dir.join("refs"),
        ] {
            println!("cargo:rerun-if-changed={}", candidate.display());
        }
    }
}
