#[derive(Debug, Clone)]
pub struct NerdExecutor {
    pub base_image: String,
}

impl NerdExecutor {
    pub fn new(base_image: String) -> Self {
        Self { base_image }
    }
}
