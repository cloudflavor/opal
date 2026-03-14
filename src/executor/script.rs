use crate::gitlab::Job;
use crate::naming::{escape_double_quotes, job_name_slug};
use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::core::CONTAINER_WORKDIR;

pub fn write_job_script(
    scripts_dir: &Path,
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
    writeln!(file, "set -eu")?;
    writeln!(file, "cd {}", CONTAINER_WORKDIR)?;
    writeln!(file)?;

    for line in commands {
        if line.trim().is_empty() {
            continue;
        }
        if verbose {
            writeln!(file, "printf '+ %s\\n' \"{}\"", escape_double_quotes(line))?;
        }
        writeln!(file, "{}", line)?;
    }

    Ok(script_path)
}
