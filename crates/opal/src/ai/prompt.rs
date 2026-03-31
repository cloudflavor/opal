use super::AiContext;
use crate::config::AiSettingsConfig;
use anyhow::{Context, Result};
use include_dir::{Dir, include_dir};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

static EMBEDDED_PROMPTS: Dir<'static> = include_dir!("$OPAL_EMBEDDED_PROMPTS_DIR");

pub struct RenderedPrompt {
    pub system: Option<String>,
    pub prompt: String,
}

pub fn render_job_analysis_prompt(
    workdir: &Path,
    settings: &AiSettingsConfig,
    context: &AiContext,
) -> Result<RenderedPrompt> {
    let vars = template_vars(context);
    let system_template = load_template(
        workdir,
        settings.prompts.system_file.as_deref(),
        "system.md",
    )?;
    let prompt_template = load_template(
        workdir,
        settings.prompts.job_analysis_file.as_deref(),
        "job-analysis.md",
    )?;

    let system = render_template(&system_template, &vars).trim().to_string();
    let prompt = render_template(&prompt_template, &vars);

    Ok(RenderedPrompt {
        system: (!system.is_empty()).then_some(system),
        prompt,
    })
}

fn load_template(
    workdir: &Path,
    override_path: Option<&str>,
    embedded_name: &str,
) -> Result<String> {
    if let Some(path) = override_path.filter(|value| !value.trim().is_empty()) {
        let path = resolve_prompt_path(workdir, path);
        return fs::read_to_string(&path)
            .with_context(|| format!("failed to read AI prompt template {}", path.display()));
    }

    let file = EMBEDDED_PROMPTS
        .get_file(embedded_name)
        .with_context(|| format!("embedded AI prompt {embedded_name} not found"))?;
    file.contents_utf8()
        .map(|text| text.to_string())
        .with_context(|| format!("embedded AI prompt {embedded_name} is not valid utf-8"))
}

fn resolve_prompt_path(workdir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        workdir.join(path)
    }
}

fn template_vars(context: &AiContext) -> HashMap<&'static str, String> {
    HashMap::from([
        ("job_name", context.job_name.clone()),
        ("source_name", context.source_name.clone()),
        ("stage", context.stage.clone()),
        ("job_yaml", context.job_yaml.clone()),
        ("runner_summary", context.runner_summary.clone()),
        ("pipeline_summary", context.pipeline_summary.clone()),
        (
            "runtime_summary",
            context.runtime_summary.clone().unwrap_or_default(),
        ),
        ("log_excerpt", context.log_excerpt.clone()),
        (
            "failure_hint",
            context.failure_hint.clone().unwrap_or_default(),
        ),
    ])
}

fn render_template(template: &str, vars: &HashMap<&'static str, String>) -> String {
    let mut rendered = template.to_string();
    for (key, value) in vars {
        rendered = rendered.replace(&format!("{{{{{key}}}}}"), value);
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::{render_job_analysis_prompt, render_template};
    use crate::ai::AiContext;
    use crate::config::AiSettingsConfig;
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn render_template_replaces_known_placeholders() {
        let vars = HashMap::from([("job_name", "unit-tests".to_string())]);
        assert_eq!(render_template("Job {{job_name}}", &vars), "Job unit-tests");
    }

    #[test]
    fn render_job_analysis_prompt_uses_embedded_defaults() {
        let dir = tempdir().expect("tempdir");
        let context = AiContext {
            job_name: "unit-tests".into(),
            source_name: "unit-tests".into(),
            stage: "test".into(),
            job_yaml: "unit-tests:\n  script:\n    - cargo test".into(),
            runner_summary: "engine=container arch=arm64 vcpu=6 ram=3g".into(),
            pipeline_summary: "dependencies: fetch-sources".into(),
            runtime_summary: Some("container: opal-unit-tests-01".into()),
            log_excerpt: "error: linker failed".into(),
            failure_hint: Some("container command exited with status Some(101)".into()),
        };

        let rendered =
            render_job_analysis_prompt(dir.path(), &AiSettingsConfig::default(), &context)
                .expect("render prompt");
        assert!(rendered.prompt.contains("unit-tests"));
        assert!(rendered.prompt.contains("error: linker failed"));
        assert!(rendered.system.is_some());
    }
}
