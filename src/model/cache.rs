use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CacheSpec {
    pub key: String,
    pub paths: Vec<PathBuf>,
    pub policy: CachePolicySpec,
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
