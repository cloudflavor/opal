use crate::pipeline::VolumeMount;
use std::path::Path;

pub struct EngineCommandContext<'a> {
    pub workdir: &'a Path,
    pub container_root: &'a Path,
    pub container_script: &'a Path,
    pub container_name: &'a str,
    pub image: &'a str,
    pub mounts: &'a [VolumeMount],
    pub env_vars: &'a [(String, String)],
}
