use crate::pipeline::Job;
use crate::secrets::SecretsStore;
use owo_colors::OwoColorize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct LogFormatter<'a> {
    use_color: bool,
    line_prefix: String,
    secrets: Option<&'a SecretsStore>,
}

impl<'a> LogFormatter<'a> {
    pub fn new(use_color: bool) -> Self {
        let line_prefix = if use_color {
            format!("{}", "    │".dimmed())
        } else {
            "    │".to_string()
        };
        Self {
            use_color,
            line_prefix,
            secrets: None,
        }
    }

    pub fn with_secrets(mut self, secrets: &'a SecretsStore) -> Self {
        self.secrets = Some(secrets);
        self
    }

    pub fn line_prefix(&self) -> &str {
        &self.line_prefix
    }

    pub fn format(&self, timestamp: &str, line_no: usize, text: &str) -> String {
        let number = format!("{:04}", line_no);
        let timestamp = if self.use_color {
            format!("{}", timestamp.bold().blue())
        } else {
            timestamp.to_string()
        };
        let number = if self.use_color {
            format!("{}", number.bold().green())
        } else {
            number
        };
        let masked = if let Some(secrets) = self.secrets {
            secrets.mask_fragment(text)
        } else {
            text.into()
        };
        format!("[{} {}] {}", timestamp, number, masked)
    }
}

pub fn sanitize_fragments(line: &str) -> Vec<String> {
    expand_carriage_returns(line)
        .into_iter()
        .map(|fragment| strip_control_sequences(&fragment))
        .collect()
}

fn expand_carriage_returns(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    for fragment in line.split('\r') {
        if fragment.is_empty() {
            continue;
        }
        parts.push(fragment.to_string());
    }
    if parts.is_empty() {
        parts.push(String::new());
    }
    parts
}

fn strip_control_sequences(line: &str) -> String {
    let mut iter = line.bytes().peekable();
    let mut output = Vec::with_capacity(line.len());
    while let Some(b) = iter.next() {
        if b == 0x1b {
            match iter.peek().copied() {
                Some(b'[') => {
                    iter.next();
                    #[allow(clippy::while_let_on_iterator)]
                    while let Some(c) = iter.next() {
                        if (0x40..=0x7E).contains(&c) {
                            break;
                        }
                    }
                    continue;
                }
                Some(b']') => {
                    iter.next();
                    #[allow(clippy::while_let_on_iterator)]
                    while let Some(c) = iter.next() {
                        if c == 0x07 {
                            break;
                        }
                        if c == 0x1b && iter.peek().copied() == Some(b'\\') {
                            iter.next();
                            break;
                        }
                    }
                    continue;
                }
                Some(_) => {
                    iter.next();
                    continue;
                }
                None => break,
            }
        } else if b == b'\x08' {
            output.pop();
        } else {
            output.push(b);
        }
    }

    String::from_utf8_lossy(&output).into_owned()
}

pub fn job_log_info(logs_dir: &Path, run_id: &str, job: &Job) -> (PathBuf, String) {
    let mut hasher = Sha256::new();
    hasher.update(run_id.as_bytes());
    hasher.update(job.stage.as_bytes());
    hasher.update(job.name.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{:x}", digest);
    let short = &hex[..12];
    let log_path = logs_dir.join(format!("{short}.log"));
    (log_path, short.to_string())
}
