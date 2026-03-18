use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct ArtifactSpec {
    pub paths: Vec<PathBuf>,
}
