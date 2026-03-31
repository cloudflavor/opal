use crate::GitLabRemoteConfig;
use crate::gitlab::PipelineGraph;
use crate::model::{
    JobSpec, PipelineDefaultsSpec, PipelineFilterSpec, PipelineSpec, StageSpec, WorkflowSpec,
};
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::path::Path;

impl PipelineSpec {
    pub fn from_path(path: &Path) -> Result<Self> {
        Self::from_path_with_gitlab(path, None)
    }

    pub fn from_path_with_gitlab(path: &Path, gitlab: Option<&GitLabRemoteConfig>) -> Result<Self> {
        let graph = PipelineGraph::from_path_with_gitlab(path, gitlab)?;
        Self::try_from(&graph)
    }
}

impl TryFrom<&PipelineGraph> for PipelineSpec {
    type Error = anyhow::Error;

    fn try_from(graph: &PipelineGraph) -> Result<Self> {
        let mut jobs = HashMap::new();
        let mut stages = Vec::with_capacity(graph.stages.len());

        // TODO: again some bullshit for for for for - refactor

        for stage in &graph.stages {
            let mut stage_jobs = Vec::with_capacity(stage.jobs.len());
            for node_idx in &stage.jobs {
                let job = graph
                    .graph
                    .node_weight(*node_idx)
                    .ok_or_else(|| anyhow!("missing job for stage '{}'", stage.name))?;
                stage_jobs.push(job.name.clone());
                jobs.insert(job.name.clone(), JobSpec::from(job));
            }
            stages.push(StageSpec {
                name: stage.name.clone(),
                jobs: stage_jobs,
            });
        }

        Ok(PipelineSpec {
            stages,
            jobs,
            defaults: PipelineDefaultsSpec::from(&graph.defaults),
            workflow: graph.workflow.as_ref().map(WorkflowSpec::from),
            filters: PipelineFilterSpec::from(&graph.filters),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::gitlab::PipelineGraph;
    use crate::model::{ParallelConfigSpec, PipelineSpec};
    use std::path::{Path, PathBuf};

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../pipelines/tests")
            .join(name)
    }

    #[test]
    fn lowers_pipeline_graph_to_pipeline_spec() {
        let fixture = fixture_path("needs-and-artifacts.gitlab-ci.yml");
        let graph = PipelineGraph::from_path(&fixture).expect("pipeline parses");
        let spec = PipelineSpec::from_path(&fixture).expect("pipeline lowers");

        assert_eq!(spec.stages.len(), graph.stages.len());
        assert!(spec.jobs.contains_key("build-matrix"));
        assert!(matches!(
            spec.jobs
                .get("build-matrix")
                .expect("job exists")
                .parallel
                .as_ref(),
            Some(ParallelConfigSpec::Matrix(_))
        ));
    }

    #[test]
    fn lowers_default_cache_fallback_keys_into_pipeline_spec() {
        let spec = PipelineSpec::from_path(&fixture_path("cache-fallback.gitlab-ci.yml"))
            .expect("pipeline lowers");

        assert_eq!(
            spec.defaults.cache[0].fallback_keys,
            vec![
                "$CACHE_NAMESPACE-$CI_DEFAULT_BRANCH".to_string(),
                "$CACHE_NAMESPACE-default".to_string()
            ]
        );
        assert_eq!(
            spec.jobs["verify-fallback-cache"].cache[0].fallback_keys,
            vec![
                "$CACHE_NAMESPACE-$CI_DEFAULT_BRANCH".to_string(),
                "$CACHE_NAMESPACE-default".to_string()
            ]
        );
    }
}
