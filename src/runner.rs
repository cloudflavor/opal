use crate::gitlab::Job;
use crate::pipeline::VolumeMount;
use crate::ui::UiBridge;
use std::path::Path;

pub struct ExecuteContext<'a> {
    pub script_path: &'a Path,
    pub log_path: &'a Path,
    pub mounts: &'a [VolumeMount],
    pub image: &'a str,
    pub container_name: &'a str,
    pub job: &'a Job,
    pub ui: Option<&'a UiBridge>,
    pub env_vars: &'a [(String, String)],
    pub network: Option<&'a str>,
}
