use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let workspace_root = manifest_dir.join("../..");
    let fallback_root = manifest_dir.join("embedded");
    let docs_src = preferred_dir(workspace_root.join("docs"), fallback_root.join("docs"));
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

    println!("cargo:rerun-if-changed={}", docs_src.display());
    println!("cargo:rerun-if-changed={}", prompts_src.display());
    println!(
        "cargo:rerun-if-changed={}",
        fallback_root.join("docs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        fallback_root.join("prompts").join("ai").display()
    );
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
