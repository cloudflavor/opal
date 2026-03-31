#[derive(Debug, Clone)]
pub struct AiContext {
    pub job_name: String,
    pub source_name: String,
    pub stage: String,
    pub job_yaml: String,
    pub runner_summary: String,
    pub pipeline_summary: String,
    pub runtime_summary: Option<String>,
    pub log_excerpt: String,
    pub failure_hint: Option<String>,
}

impl AiContext {
    pub fn to_prompt(&self, system: Option<&str>) -> String {
        let mut prompt = String::new();
        if let Some(system) = system.filter(|value| !value.trim().is_empty()) {
            prompt.push_str(system.trim());
            prompt.push_str("\n\n");
        }
        prompt.push_str("You are helping troubleshoot a local GitLab-style CI job run in Opal. ");
        prompt.push_str("Be concise, name the most likely root cause first, and then list concrete next debugging or fix steps. ");
        prompt.push_str("If the evidence is inconclusive, say what extra signal is missing.\n\n");
        prompt.push_str(&format!("Job: {}\n", self.job_name));
        prompt.push_str(&format!("Source job: {}\n", self.source_name));
        prompt.push_str(&format!("Stage: {}\n", self.stage));
        prompt.push_str(&format!("Runner: {}\n\n", self.runner_summary));

        if let Some(hint) = &self.failure_hint {
            prompt.push_str("Current failure summary:\n");
            prompt.push_str(hint.trim());
            prompt.push_str("\n\n");
        }

        prompt.push_str("Selected job YAML:\n```yaml\n");
        prompt.push_str(self.job_yaml.trim());
        prompt.push_str("\n```\n\n");

        prompt.push_str("Pipeline plan summary:\n```text\n");
        prompt.push_str(self.pipeline_summary.trim());
        prompt.push_str("\n```\n\n");

        if let Some(runtime) = &self.runtime_summary {
            prompt.push_str("Runtime summary:\n```text\n");
            prompt.push_str(runtime.trim());
            prompt.push_str("\n```\n\n");
        }

        prompt.push_str("Recent job log excerpt:\n```text\n");
        prompt.push_str(self.log_excerpt.trim());
        prompt.push_str("\n```\n\n");

        prompt.push_str("Respond with:\n");
        prompt.push_str("1. Root cause\n");
        prompt.push_str("2. Why you think that\n");
        prompt.push_str("3. Concrete next steps\n");
        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::AiContext;

    #[test]
    fn prompt_contains_core_troubleshooting_sections() {
        let context = AiContext {
            job_name: "unit-tests".to_string(),
            source_name: "unit-tests".to_string(),
            stage: "test".to_string(),
            job_yaml: "unit-tests:\n  script:\n    - cargo test".to_string(),
            runner_summary: "engine=container arch=arm64 vcpu=6 ram=3g".to_string(),
            pipeline_summary: "dependencies: fetch-sources, rust-checks".to_string(),
            runtime_summary: Some("container: opal-unit-tests-01".to_string()),
            log_excerpt: "error: linker failed".to_string(),
            failure_hint: Some("container command exited with status Some(101)".to_string()),
        };

        let prompt = context.to_prompt(Some("system"));
        assert!(prompt.contains("system"));
        assert!(prompt.contains("Selected job YAML"));
        assert!(prompt.contains("Recent job log excerpt"));
        assert!(prompt.contains("Root cause"));
        assert!(prompt.contains("container command exited with status Some(101)"));
    }
}
