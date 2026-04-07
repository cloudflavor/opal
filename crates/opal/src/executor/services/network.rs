use super::command::{command_timeout, engine_binary, run_command_with_timeout};
use anyhow::{Result, anyhow};
use std::process::Command;
use std::thread;
use std::time::Duration;
use tracing::warn;

const CONTAINER_NETWORK_RETRY_ATTEMPTS: usize = 8;
const CONTAINER_NETWORK_RETRY_DELAY_MS: u64 = 750;

pub(super) struct ServiceNetworkManager {
    engine: crate::EngineKind,
}

impl ServiceNetworkManager {
    pub(super) fn new(engine: crate::EngineKind) -> Self {
        Self { engine }
    }

    pub(super) fn create(&self, network: &str) -> Result<()> {
        self.run("create", network)
    }

    pub(super) fn remove(&self, network: &str) -> Result<()> {
        self.run("rm", network)
    }

    fn run(&self, action: &str, network: &str) -> Result<()> {
        let attempts = if matches!(self.engine, crate::EngineKind::ContainerCli) {
            CONTAINER_NETWORK_RETRY_ATTEMPTS
        } else {
            1
        };

        let mut last_error = None;
        for attempt in 0..attempts {
            let mut command = Command::new(engine_binary(self.engine));
            command.arg("network").arg(action).arg(network);

            match run_command_with_timeout(command, command_timeout(self.engine)) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    if matches!(self.engine, crate::EngineKind::ContainerCli)
                        && should_retry_container_network_error(&err.to_string())
                        && attempt + 1 < attempts
                    {
                        warn!(
                            network,
                            action,
                            attempt = attempt + 1,
                            "container network command timed out; retrying"
                        );
                        thread::sleep(Duration::from_millis(
                            CONTAINER_NETWORK_RETRY_DELAY_MS * (attempt + 1) as u64,
                        ));
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

pub(super) fn should_retry_container_network_error(message: &str) -> bool {
    message.contains("XPC timeout for request to com.apple.container.apiserver/networkCreate")
        || message
            .contains("XPC timeout for request to com.apple.container.apiserver/networkDelete")
        || message.contains("Connection invalid")
        || message.contains("apiserver")
}
