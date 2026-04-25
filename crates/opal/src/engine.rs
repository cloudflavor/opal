use crate::pipeline::VolumeMount;
use std::path::Path;

pub struct EngineCommandContext<'a> {
    pub workdir: &'a Path,
    pub container_root: &'a Path,
    pub container_script: &'a Path,
    pub container_name: &'a str,
    pub image: &'a str,
    pub image_platform: Option<&'a str>,
    pub image_user: Option<&'a str>,
    pub image_entrypoint: &'a [String],
    pub mounts: &'a [VolumeMount],
    pub env_vars: &'a [(String, String)],
    pub host_aliases: &'a [(String, String)],
    pub network: Option<&'a str>,
    pub preserve_runtime_objects: bool,
    pub arch: Option<&'a str>,
    pub privileged: bool,
    pub cap_add: &'a [String],
    pub cap_drop: &'a [String],
    pub cpus: Option<&'a str>,
    pub memory: Option<&'a str>,
    pub dns: Option<&'a str>,
}
