use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct JobDependencySpec {
    pub job: String,
    pub needs_artifacts: bool,
    pub optional: bool,
    pub source: DependencySourceSpec,
    pub parallel: Option<Vec<HashMap<String, String>>>,
    pub inline_variant: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub enum DependencySourceSpec {
    Local,
    External(ExternalDependencySpec),
}

#[derive(Debug, Clone)]
pub struct ExternalDependencySpec {
    pub project: String,
    pub reference: String,
}
