use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct ArtifactSpec {
    pub paths: Vec<PathBuf>,
    pub exclude: Vec<String>,
    pub untracked: bool,
    pub when: ArtifactWhenSpec,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ArtifactWhenSpec {
    #[default]
    OnSuccess,
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactSourceOutcome {
    Success,
    Failed,
    Skipped,
}

impl ArtifactWhenSpec {
    pub fn includes(self, outcome: Option<ArtifactSourceOutcome>) -> bool {
        match self {
            ArtifactWhenSpec::Always => true,
            ArtifactWhenSpec::OnSuccess => matches!(outcome, Some(ArtifactSourceOutcome::Success)),
            ArtifactWhenSpec::OnFailure => matches!(outcome, Some(ArtifactSourceOutcome::Failed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ArtifactSourceOutcome, ArtifactWhenSpec};

    #[test]
    fn artifact_when_matches_expected_outcomes() {
        assert!(ArtifactWhenSpec::Always.includes(None));
        assert!(ArtifactWhenSpec::Always.includes(Some(ArtifactSourceOutcome::Success)));
        assert!(ArtifactWhenSpec::OnSuccess.includes(Some(ArtifactSourceOutcome::Success)));
        assert!(!ArtifactWhenSpec::OnSuccess.includes(Some(ArtifactSourceOutcome::Failed)));
        assert!(ArtifactWhenSpec::OnFailure.includes(Some(ArtifactSourceOutcome::Failed)));
        assert!(!ArtifactWhenSpec::OnFailure.includes(Some(ArtifactSourceOutcome::Skipped)));
        assert!(!ArtifactWhenSpec::OnFailure.includes(None));
    }
}
