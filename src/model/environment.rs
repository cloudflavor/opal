use std::time::Duration;

#[derive(Debug, Clone)]
pub struct EnvironmentSpec {
    pub name: String,
    pub url: Option<String>,
    pub on_stop: Option<String>,
    pub auto_stop_in: Option<Duration>,
    pub action: EnvironmentActionSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentActionSpec {
    Start,
    Stop,
}
