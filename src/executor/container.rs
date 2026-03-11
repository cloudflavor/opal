#[derive(Debug, Clone)]
pub struct ContainerExecutor {
    pub base_image: String,
}

impl ContainerExecutor {
    pub fn new(base_image: String) -> Self {
        Self { base_image }
    }
}
