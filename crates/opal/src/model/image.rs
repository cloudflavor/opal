#[derive(Debug, Clone, Default)]
pub struct ImageSpec {
    pub name: String,
    pub docker_platform: Option<String>,
    pub docker_user: Option<String>,
    pub entrypoint: Vec<String>,
}

impl From<String> for ImageSpec {
    fn from(name: String) -> Self {
        Self {
            name,
            docker_platform: None,
            docker_user: None,
            entrypoint: Vec::new(),
        }
    }
}

impl From<&str> for ImageSpec {
    fn from(name: &str) -> Self {
        Self::from(name.to_string())
    }
}
