use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CacheSpec {
    pub key: CacheKeySpec,
    pub fallback_keys: Vec<String>,
    pub paths: Vec<PathBuf>,
    pub policy: CachePolicySpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheKeySpec {
    Literal(String),
    Files {
        files: Vec<PathBuf>,
        prefix: Option<String>,
    },
}

impl Default for CacheKeySpec {
    fn default() -> Self {
        Self::Literal("default".to_string())
    }
}

impl CacheKeySpec {
    pub fn describe(&self) -> String {
        match self {
            CacheKeySpec::Literal(value) => value.clone(),
            CacheKeySpec::Files { files, prefix } => {
                let files_text = files
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                if let Some(prefix) = prefix {
                    format!("{{ files: [{files_text}], prefix: {prefix} }}")
                } else {
                    format!("{{ files: [{files_text}] }}")
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicySpec {
    Pull,
    Push,
    PullPush,
}

impl CachePolicySpec {
    pub fn allows_pull(self) -> bool {
        matches!(self, Self::Pull | Self::PullPush)
    }

    pub fn allows_push(self) -> bool {
        matches!(self, Self::Push | Self::PullPush)
    }
}
