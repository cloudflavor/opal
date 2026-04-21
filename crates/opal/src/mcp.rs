mod protocol;
mod resources;
mod server;
mod tools;
mod uri;

use crate::app::OpalApp;
use anyhow::Result;
use serde_json::Value;

pub use server::serve_stdio;

pub(crate) async fn call_tool(app: &OpalApp, params: Value) -> Result<Value> {
    tools::call_tool(app, params).await
}

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
