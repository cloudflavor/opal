use super::OpalApp;
use crate::OperationsArgs;
use anyhow::{Context, Result};
use serde_json::{Value, json};

pub(crate) async fn execute(app: &OpalApp, args: OperationsArgs) -> Result<()> {
    let mut arguments = json!({
        "active_only": !args.all,
        "limit": args.limit,
    });
    if let Some(status) = args.status {
        arguments["status"] = json!(status.as_str());
    }

    let response = crate::mcp::call_tool(
        app,
        json!({
            "name": "opal_operations_list",
            "arguments": arguments,
        }),
    )
    .await?;

    let message = response
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("operation listing failed");
    if response
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        anyhow::bail!(message.to_string());
    }

    if args.json {
        let payload = response
            .get("structuredContent")
            .cloned()
            .unwrap_or(Value::Null);
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).context("failed to encode operation payload")?
        );
    } else {
        println!("{message}");
    }
    Ok(())
}
