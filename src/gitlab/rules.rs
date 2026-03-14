use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct JobRule {
    #[serde(rename = "if")]
    pub if_expr: Option<String>,
    pub changes: Option<RuleChangesRaw>,
    pub exists: Option<RuleExistsRaw>,
    pub when: Option<String>,
    pub allow_failure: Option<bool>,
    pub start_in: Option<String>,
    #[serde(default)]
    pub variables: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RuleChangesRaw {
    Simple(Vec<String>),
    Nested {
        paths: Vec<String>,
        compare_to: Option<String>,
    },
}

impl Default for RuleChangesRaw {
    fn default() -> Self {
        RuleChangesRaw::Simple(Vec::new())
    }
}

impl RuleChangesRaw {
    pub fn paths(&self) -> &[String] {
        match self {
            RuleChangesRaw::Simple(paths) => paths,
            RuleChangesRaw::Nested { paths, .. } => paths,
        }
    }

    pub fn compare_to(&self) -> Option<&str> {
        match self {
            RuleChangesRaw::Simple(_) => None,
            RuleChangesRaw::Nested { compare_to, .. } => compare_to.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RuleExistsRaw {
    Simple(Vec<String>),
    Nested { paths: Vec<String> },
}

impl Default for RuleExistsRaw {
    fn default() -> Self {
        RuleExistsRaw::Simple(Vec::new())
    }
}

impl RuleExistsRaw {
    pub fn paths(&self) -> &[String] {
        match self {
            RuleExistsRaw::Simple(paths) => paths,
            RuleExistsRaw::Nested { paths } => paths,
        }
    }
}
