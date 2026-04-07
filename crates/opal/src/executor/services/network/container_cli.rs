use anyhow::Error;
use std::time::Duration;

const CONTAINER_NETWORK_RETRY_ATTEMPTS: usize = 8;
const CONTAINER_NETWORK_RETRY_DELAY_MS: u64 = 750;

pub(super) struct ContainerCliRetryPolicy {
    attempts: usize,
}

impl ContainerCliRetryPolicy {
    fn disabled() -> Self {
        Self { attempts: 1 }
    }

    pub(super) fn attempts(&self) -> usize {
        self.attempts
    }

    pub(super) fn should_retry(&self, err: &Error, attempt: usize) -> bool {
        self.attempts > 1
            && attempt + 1 < self.attempts
            && is_retryable_apiserver_error(&err.to_string())
    }

    pub(super) fn backoff_delay(&self, attempt: usize) -> Duration {
        Duration::from_millis(CONTAINER_NETWORK_RETRY_DELAY_MS * (attempt + 1) as u64)
    }
}

pub(super) fn retry_policy(engine: crate::EngineKind) -> ContainerCliRetryPolicy {
    if matches!(engine, crate::EngineKind::ContainerCli) {
        ContainerCliRetryPolicy {
            attempts: CONTAINER_NETWORK_RETRY_ATTEMPTS,
        }
    } else {
        ContainerCliRetryPolicy::disabled()
    }
}

pub(super) fn is_retryable_apiserver_error(message: &str) -> bool {
    message.contains("XPC timeout for request to com.apple.container.apiserver/networkCreate")
        || message
            .contains("XPC timeout for request to com.apple.container.apiserver/networkDelete")
        || message.contains("Connection invalid")
        || message.contains("apiserver")
}
