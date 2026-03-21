use crate::model::JobSpec;
use crate::naming::job_name_slug;
use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn write_job_script(
    scripts_dir: &Path,
    container_workdir: &Path,
    job: &JobSpec,
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
        writeln!(file, "set -ex")?;
    } else {
        writeln!(file, "set -e")?;
    }
    writeln!(file, "cd {}", container_workdir.display())?;
    writeln!(file)?;

    for line in commands {
        if line.trim().is_empty() {
            continue;
        }
        writeln!(
            file,
            "printf '%s\\n' {}",
            shell_quote(&format!("$ {}", line))
        )?;
        writeln!(file, "{}", line)?;
        writeln!(file)?;
    }

    Ok(script_path)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::write_job_script;
    use crate::model::{ArtifactSpec, JobSpec, RetryPolicySpec};
    use std::collections::HashMap;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn writes_non_verbose_script_without_nounset() {
        let dir = tempdir().expect("tempdir");
        let script_path = write_job_script(
            dir.path(),
            Path::new("/builds/project"),
            &job(),
            &["echo hello".to_string()],
            false,
        )
        .expect("write script");
        let script = fs::read_to_string(script_path).expect("read script");
        assert!(script.contains("set -e"));
        assert!(!script.contains("set -eu"));
    }

    #[test]
    fn writes_verbose_script_without_nounset() {
        let dir = tempdir().expect("tempdir");
        let script_path = write_job_script(
            dir.path(),
            Path::new("/builds/project"),
            &job(),
            &["echo hello".to_string()],
            true,
        )
        .expect("write script");
        let script = fs::read_to_string(script_path).expect("read script");
        assert!(script.contains("set -ex"));
        assert!(!script.contains("set -eux"));
    }

    #[test]
    fn writes_script_with_command_tracing_lines() {
        let dir = tempdir().expect("tempdir");
        let script_path = write_job_script(
            dir.path(),
            Path::new("/builds/project"),
            &job(),
            &["test -n \"$QUAY_USERNAME\"".to_string()],
            false,
        )
        .expect("write script");
        let script = fs::read_to_string(script_path).expect("read script");
        assert!(script.contains("printf '%s\\n' '$ test -n \"$QUAY_USERNAME\"'"));
        assert!(script.contains("test -n \"$QUAY_USERNAME\""));
    }

    fn job() -> JobSpec {
        JobSpec {
            name: "test".into(),
            stage: "build".into(),
            commands: vec!["echo hello".into()],
            needs: Vec::new(),
            explicit_needs: false,
            dependencies: Vec::new(),
            before_script: None,
            after_script: None,
            inherit_default_before_script: true,
            inherit_default_after_script: true,
            when: None,
            rules: Vec::new(),
            only: Vec::new(),
            except: Vec::new(),
            artifacts: ArtifactSpec::default(),
            cache: Vec::new(),
            image: None,
            variables: HashMap::new(),
            services: Vec::new(),
            timeout: None,
            retry: RetryPolicySpec::default(),
            interruptible: false,
            resource_group: None,
            parallel: None,
            tags: Vec::new(),
            environment: None,
        }
    }

    use std::path::Path;
}
