use crate::gitlab::{DependencySource, Job};
use crate::pipeline::{JobPlan, JobStatus, JobSummary, VolumeMount};
use owo_colors::OwoColorize;
use std::path::{Path, PathBuf};

pub struct DisplayFormatter {
    use_color: bool,
}

impl DisplayFormatter {
    pub fn new(use_color: bool) -> Self {
        Self { use_color }
    }

    pub fn colorize<'a, F>(&self, text: &'a str, styler: F) -> String
    where
        F: FnOnce(&'a str) -> String,
    {
        if self.use_color {
            styler(text)
        } else {
            text.to_string()
        }
    }

    pub fn bold_green(&self, text: &str) -> String {
        self.colorize(text, |t| format!("{}", t.bold().green()))
    }

    pub fn bold_red(&self, text: &str) -> String {
        self.colorize(text, |t| format!("{}", t.bold().red()))
    }

    pub fn bold_yellow(&self, text: &str) -> String {
        self.colorize(text, |t| format!("{}", t.bold().yellow()))
    }

    pub fn bold_white(&self, text: &str) -> String {
        self.colorize(text, |t| format!("{}", t.bold().white()))
    }

    pub fn bold_cyan(&self, text: &str) -> String {
        self.colorize(text, |t| format!("{}", t.bold().cyan()))
    }

    pub fn bold_blue(&self, text: &str) -> String {
        self.colorize(text, |t| format!("{}", t.bold().blue()))
    }

    pub fn bold_magenta(&self, text: &str) -> String {
        self.colorize(text, |t| format!("{}", t.bold().magenta()))
    }

    pub fn stage_header(&self, name: &str) -> String {
        let prefix = self.bold_blue("╭────────");
        let stage_text = format!("stage {name}");
        let stage = self.bold_magenta(&stage_text);
        let suffix = self.bold_blue("────────╮");
        format!("{} {} {}", prefix, stage, suffix)
    }

    pub fn logs_header(&self) -> String {
        self.bold_cyan("    logs:")
    }

    pub fn format_needs(&self, job: &Job) -> Option<String> {
        if job.needs.is_empty() {
            return None;
        }

        let entries: Vec<String> = job
            .needs
            .iter()
            .map(|need| match &need.source {
                DependencySource::Local => {
                    if need.needs_artifacts {
                        format!("{} (artifacts)", need.job)
                    } else {
                        need.job.clone()
                    }
                }
                DependencySource::External(ext) => {
                    let mut label = format!("{}::{}", ext.project, need.job);
                    if need.needs_artifacts {
                        label.push_str(" (external artifacts)");
                    } else {
                        label.push_str(" (external)");
                    }
                    label
                }
            })
            .collect();

        Some(entries.join(", "))
    }

    pub fn format_paths(&self, paths: &[PathBuf]) -> Option<String> {
        if paths.is_empty() {
            return None;
        }

        Some(
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    pub fn format_mounts(&self, mounts: &[VolumeMount]) -> String {
        let label = self.bold_cyan("    artifact mounts:");
        if mounts.is_empty() {
            return format!("{} none", label);
        }

        let desc = mounts
            .iter()
            .map(|mount| mount.container.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");

        format!("{} {}", label, desc)
    }
}

pub fn indent_block(block: &str, prefix: &str) -> String {
    let mut buf = String::new();
    for (idx, line) in block.lines().enumerate() {
        if idx > 0 {
            buf.push('\n');
        }
        buf.push_str(prefix);
        buf.push_str(line);
    }
    buf
}

pub fn print_line(line: impl AsRef<str>) {
    println!("{}", line.as_ref());
}

pub fn print_blank_line() {
    println!();
}

pub fn print_prefixed_line(prefix: &str, line: &str) {
    println!("{} {}", prefix, line);
}

pub fn print_pipeline_summary<F>(
    display: &DisplayFormatter,
    plan: &JobPlan,
    summaries: &[JobSummary],
    session_dir: &Path,
    mut emit_line: F,
) where
    F: FnMut(String),
{
    emit_line(String::new());
    let header = display.bold_blue("╭─ pipeline summary");
    emit_line(header);

    if summaries.is_empty() {
        emit_line("  no jobs were executed".to_string());
        emit_line(format!("  session data: {}", session_dir.display()));
        return;
    }

    let mut ordered = summaries.to_vec();
    ordered.sort_by_key(|entry| {
        plan.order_index
            .get(&entry.name)
            .copied()
            .unwrap_or(usize::MAX)
    });

    let mut success = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut allowed_failures = 0usize;

    for entry in &ordered {
        match &entry.status {
            JobStatus::Success => success += 1,
            JobStatus::Failed(_) => {
                if entry.allow_failure {
                    allowed_failures += 1;
                } else {
                    failed += 1;
                }
            }
            JobStatus::Skipped(_) => skipped += 1,
        }
        let icon = match &entry.status {
            JobStatus::Success => display.bold_green("✓"),
            JobStatus::Failed(_) if entry.allow_failure => display.bold_yellow("!"),
            JobStatus::Failed(_) => display.bold_red("✗"),
            JobStatus::Skipped(_) => display.bold_yellow("•"),
        };
        let mut line = format!(
            "  {} {} (stage {}, log {})",
            icon, entry.name, entry.stage_name, entry.log_hash
        );
        match &entry.status {
            JobStatus::Success => line.push_str(&format!(" – {:.2}s", entry.duration)),
            JobStatus::Failed(msg) => {
                if entry.allow_failure {
                    line.push_str(&format!(
                        " – {:.2}s failed (allowed): {}",
                        entry.duration, msg
                    ));
                } else {
                    line.push_str(&format!(" – {:.2}s failed: {}", entry.duration, msg));
                }
            }
            JobStatus::Skipped(msg) => line.push_str(&format!(" – {}", msg)),
        }
        if let Some(log_path) = &entry.log_path {
            line.push_str(&format!(" [log: {}]", log_path.display()));
        }
        emit_line(line);
    }

    let mut summary_line = format!(
        "  results: {} ok / {} failed / {} skipped",
        success, failed, skipped
    );
    if allowed_failures > 0 {
        summary_line.push_str(&format!(" / {} allowed", allowed_failures));
    }
    emit_line(summary_line);
    emit_line(format!("  session data: {}", session_dir.display()));
}
