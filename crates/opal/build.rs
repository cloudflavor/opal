use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        compute_build_version(&workspace_root)
    );

    println!("cargo:rerun-if-changed={}", docs_src.display());
    println!("cargo:rerun-if-changed={}", prompts_src.display());
    println!(
        "cargo:rerun-if-changed={}",
        fallback_root.join("prompts").join("ai").display()
    );
    emit_git_rerun_hints(&workspace_root);
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

fn compute_build_version(workspace_root: &Path) -> String {
    let package_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| String::from("0.0.0"));
    let dirty = is_workspace_dirty(workspace_root).unwrap_or(false);

    if let Some(describe) = git_stdout(
        workspace_root,
        &[
            "describe",
            "--tags",
            "--match",
            "v[0-9]*.[0-9]*.[0-9]*",
            "--long",
            "--abbrev=7",
            "HEAD",
        ],
    ) && let Some(version) = version_scheme::version_from_git_describe(&describe, dirty)
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

    let short_sha = git_stdout(workspace_root, &["rev-parse", "--short=7", "HEAD"]);
    version_scheme::fallback_version(&package_version, short_sha.as_deref(), dirty)
}

fn is_workspace_dirty(workspace_root: &Path) -> Option<bool> {
    let status = git_stdout(
        workspace_root,
        &["status", "--porcelain", "--untracked-files=normal"],
    )?;
    Some(!status.is_empty())
}

fn git_stdout(workspace_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    Some(stdout.trim().to_string())
}

fn emit_git_rerun_hints(workspace_root: &Path) {
    for candidate in [
        workspace_root.join(".git"),
        workspace_root.join(".git").join("HEAD"),
        workspace_root.join(".git").join("index"),
        workspace_root.join(".git").join("packed-refs"),
        workspace_root.join(".git").join("refs"),
    ] {
        println!("cargo:rerun-if-changed={}", candidate.display());
    }

    for git_path in ["HEAD", "index", "packed-refs", "refs"] {
        if let Some(path) = git_stdout(workspace_root, &["rev-parse", "--git-path", git_path]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
