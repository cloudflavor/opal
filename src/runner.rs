use crate::model::JobSpec;
use crate::pipeline::VolumeMount;
use crate::ui::UiBridge;
use std::path::Path;

pub struct ExecuteContext<'a> {
    pub host_workdir: &'a Path,
    pub script_path: &'a Path,
    pub log_path: &'a Path,
    pub mounts: &'a [VolumeMount],
    pub image: &'a str,
    pub image_platform: Option<&'a str>,
    pub image_user: Option<&'a str>,
    pub image_entrypoint: &'a [String],
    pub container_name: &'a str,
    pub job: &'a JobSpec,
    pub ui: Option<&'a UiBridge>,
    pub env_vars: &'a [(String, String)],
    pub network: Option<&'a str>,
    pub arch: Option<&'a str>,
    pub privileged: bool,
    pub cap_add: &'a [String],
    pub cap_drop: &'a [String],
}
