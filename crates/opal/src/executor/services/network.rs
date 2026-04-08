mod container_cli;

use super::command::{command_timeout, engine_binary, run_command_with_timeout};
use anyhow::{Result, anyhow};
use std::process::Command;
use tokio::time::sleep;
use tracing::warn;

pub(super) struct ServiceNetworkManager {
    engine: crate::EngineKind,
}

impl ServiceNetworkManager {
    pub(super) fn new(engine: crate::EngineKind) -> Self {
        Self { engine }
    }

    pub(super) async fn create(&self, network: &str) -> Result<()> {
        self.run("create", network).await
    }

    pub(super) async fn remove(&self, network: &str) -> Result<()> {
        self.run("rm", network).await
    }

    async fn run(&self, action: &str, network: &str) -> Result<()> {
        let retry_policy = container_cli::retry_policy(self.engine);

        let mut last_error = None;
        for attempt in 0..retry_policy.attempts() {
            let mut command = Command::new(engine_binary(self.engine));
            command.arg("network").arg(action).arg(network);
            let command_line = describe_command(&command);

            match run_command_with_timeout(command, command_timeout(self.engine)).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let err = anyhow!("container network command failed: {command_line}: {err}");
                    if retry_policy.should_retry(&err, attempt) {
                        warn!(
                            network,
                            action,
                            attempt = attempt + 1,
                            "container network command timed out; retrying"
                        );
                        sleep(retry_policy.backoff_delay(attempt)).await;
                        last_error = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("network command failed without an error")))
    }
}

fn describe_command(command: &Command) -> String {
    let mut args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let program = command.get_program().to_string_lossy();
    if args.is_empty() {
        return program.into_owned();
    }
    args.insert(0, program.into_owned());
    args.join(" ")
}

#[cfg(test)]
pub(super) fn should_retry_container_network_error(message: &str) -> bool {
    container_cli::is_retryable_apiserver_error(message)
}
