use crate::gitlab::Job;
use crate::naming::job_name_slug;
use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn write_job_script(
    scripts_dir: &Path,
    container_workdir: &Path,
    job: &Job,
    commands: &[String],
    verbose: bool,
) -> Result<PathBuf> {
    let slug = job_name_slug(&job.name);
    let script_path = scripts_dir.join(format!("{slug}.sh"));
    if let Some(parent) = script_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("failed to create dir {:?}", parent))?;
    }

    let mut file = File::create(&script_path)
        .with_context(|| format!("failed to create script for {}", job.name))?;
    writeln!(file, "#!/usr/bin/env sh")?;
    if verbose {
        writeln!(file, "set -eux")?;
    } else {
        writeln!(file, "set -eu")?;
    }
    writeln!(file, "cd {}", container_workdir.display())?;
    writeln!(file)?;

    for line in commands {
        if line.trim().is_empty() {
            continue;
        }
        writeln!(file, "{}", line)?;
    }

    Ok(script_path)
}
