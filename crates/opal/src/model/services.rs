use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ServiceSpec {
    pub image: String,
    pub aliases: Vec<String>,
    pub docker_platform: Option<String>,
    pub docker_user: Option<String>,
    pub entrypoint: Vec<String>,
    pub command: Vec<String>,
    pub variables: HashMap<String, String>,
}
