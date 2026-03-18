use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct ServiceSpec {
    pub image: String,
    pub alias: Option<String>,
    pub entrypoint: Vec<String>,
    pub command: Vec<String>,
    pub variables: HashMap<String, String>,
}
