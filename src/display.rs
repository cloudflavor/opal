use crate::gitlab::{
    CacheConfig, CachePolicy, DependencySource, EnvironmentAction, Job, JobDependency,
};
use crate::pipeline::{JobPlan, JobStatus, JobSummary, PlannedJob, RuleWhen, VolumeMount};
use ascii_tree::{Tree, write_tree};
use humantime::format_duration;
use owo_colors::OwoColorize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
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
        if let Some(env) = &entry.environment {
            line.push_str(&format!(" (env: {}", env.name));
            if let Some(url) = &env.url {
                line.push_str(&format!(", url: {}", url));
            }
            line.push(')');
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

pub fn print_pipeline_plan<F>(display: &DisplayFormatter, plan: &JobPlan, mut emit_line: F)
where
    F: FnMut(String),
{
    emit_line(String::new());
    let header = display.bold_blue("╭─ pipeline plan");
    emit_line(header);
    if plan.ordered.is_empty() {
        emit_line("  no jobs scheduled for this context".to_string());
        return;
    }

    let mut current_stage: Option<String> = None;
    for job_name in &plan.ordered {
        let Some(planned) = plan.nodes.get(job_name) else {
            continue;
        };
        if current_stage.as_deref() != Some(planned.stage_name.as_str()) {
            current_stage = Some(planned.stage_name.clone());
            emit_line(String::new());
            emit_line(display.stage_header(&planned.stage_name));
        }
        emit_plan_job(display, plan, planned, &mut emit_line);
    }
}

pub fn collect_pipeline_plan(display: &DisplayFormatter, plan: &JobPlan) -> Vec<String> {
    let mut lines = Vec::new();
    print_pipeline_plan(display, plan, |line| lines.push(line));
    while matches!(lines.first(), Some(existing) if existing.trim().is_empty()) {
        lines.remove(0);
    }
    lines
}

fn emit_plan_job<F>(
    display: &DisplayFormatter,
    plan: &JobPlan,
    planned: &PlannedJob,
    emit_line: &mut F,
) where
    F: FnMut(String),
{
    let job_label = display.bold_green("  job:");
    let job_name = display.bold_white(planned.job.name.as_str());
    emit_line(format!("{job_label} {job_name}"));

    if let Some(meta) = plan_job_meta(planned) {
        emit_line(format!("{} {}", display.bold_cyan("    info:"), meta));
    }

    emit_section(
        display,
        "depends on",
        &plan_dependency_lines(planned),
        emit_line,
    );
    emit_section(display, "needs", &plan_needs_lines(&planned.job), emit_line);
    emit_section(
        display,
        "artifact downloads",
        &plan_dependencies_list(&planned.job),
        emit_line,
    );

    if let Some(paths) = display.format_paths(&planned.job.artifacts) {
        emit_line(format!("{} {}", display.bold_cyan("    artifacts:"), paths));
    }
    emit_section(
        display,
        "caches",
        &plan_cache_lines(&planned.job),
        emit_line,
    );

    if let Some(tree_lines) = plan_relationship_tree_lines(plan, planned) {
        emit_line(String::new());
        let header = display.bold_cyan("    relationships graph");
        emit_line(format!("{header}:"));
        for line in tree_lines {
            emit_line(format!("    {line}"));
        }
    }

    if let Some(env_line) = format_environment(planned) {
        emit_line(format!(
            "{} {}",
            display.bold_cyan("    environment:"),
            env_line
        ));
    }

    if let Some(timeout) = planned.timeout {
        emit_line(format!(
            "{} {}",
            display.bold_cyan("    timeout:"),
            format_duration(timeout)
        ));
    }

    if let Some(group) = &planned.resource_group {
        emit_line(format!(
            "{} {}",
            display.bold_cyan("    resource group:"),
            group
        ));
    }
}

fn plan_job_meta(planned: &PlannedJob) -> Option<String> {
    let mut parts = Vec::new();
    parts.push(format!("when {}", describe_rule_when(planned.rule.when)));
    if planned.rule.allow_failure {
        parts.push("allow failure".to_string());
    }
    if let Some(delay) = planned.rule.start_in {
        parts.push(format!("start after {}", format_duration(delay)));
    }
    if planned.rule.when == RuleWhen::Manual {
        if planned.rule.manual_auto_run {
            parts.push("auto-run manual".to_string());
        } else {
            parts.push("requires trigger".to_string());
        }
        if let Some(reason) = &planned.rule.manual_reason
            && !reason.is_empty()
        {
            parts.push(format!("reason: {}", reason));
        }
    }
    if planned.job.retry.max > 0 {
        parts.push(format!("retries {}", planned.job.retry.max));
    }
    if planned.job.interruptible {
        parts.push("interruptible".to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" • "))
    }
}

fn describe_rule_when(when: RuleWhen) -> &'static str {
    match when {
        RuleWhen::OnSuccess => "on_success",
        RuleWhen::Manual => "manual",
        RuleWhen::Delayed => "delayed",
        RuleWhen::Never => "never",
        RuleWhen::Always => "always",
        RuleWhen::OnFailure => "on_failure",
    }
}

fn format_environment(planned: &PlannedJob) -> Option<String> {
    let env = planned.job.environment.as_ref()?;
    let mut parts = Vec::new();
    parts.push(env.name.clone());
    if let Some(url) = &env.url {
        parts.push(url.clone());
    }
    let mut extra = Vec::new();
    if let Some(on_stop) = &env.on_stop {
        extra.push(format!("on_stop: {}", on_stop));
    }
    if let Some(duration) = env.auto_stop_in {
        extra.push(format!("auto_stop {}", format_duration(duration)));
    }
    if env.action == EnvironmentAction::Stop {
        extra.push("stop".to_string());
    }
    if !extra.is_empty() {
        parts.push(extra.join(", "));
    }
    Some(parts.join(" – "))
}

fn plan_dependency_lines(planned: &PlannedJob) -> Vec<String> {
    if planned.dependencies.is_empty() {
        return vec!["stage ordering".to_string()];
    }
    let needs_map = planned
        .job
        .needs
        .iter()
        .map(|need| (need.job.clone(), need.needs_artifacts))
        .collect::<HashMap<String, bool>>();
    planned
        .dependencies
        .iter()
        .map(|name| {
            if let Some(artifacts) = needs_map.get(name) {
                if *artifacts {
                    format!("• {name} (needs + artifacts)")
                } else {
                    format!("• {name} (needs)")
                }
            } else {
                format!("• {name} (previous stage)")
            }
        })
        .collect()
}

fn plan_needs_lines(job: &Job) -> Vec<String> {
    if job.needs.is_empty() {
        return Vec::new();
    }
    let dependency_set: HashSet<&str> = job.dependencies.iter().map(|s| s.as_str()).collect();
    job.needs
        .iter()
        .map(|need| format_need_line(need, dependency_set.contains(need.job.as_str())))
        .collect()
}

fn format_need_line(need: &JobDependency, has_dependency: bool) -> String {
    let mut tags = Vec::new();
    match &need.source {
        DependencySource::Local => tags.push("local".to_string()),
        DependencySource::External(ext) => tags.push(format!("external {}", ext.project)),
    }
    if need.needs_artifacts {
        tags.push("artifacts".to_string());
    }
    if need.optional {
        tags.push("optional".to_string());
    }
    if has_dependency {
        tags.push("downloads via dependencies".to_string());
    }
    if let Some(filters) = &need.parallel
        && !filters.is_empty()
    {
        tags.push("matrix filter".to_string());
    }
    if tags.is_empty() {
        format!("• {}", need.job)
    } else {
        format!("• {} ({})", need.job, tags.join(", "))
    }
}

fn plan_dependencies_list(job: &Job) -> Vec<String> {
    if job.dependencies.is_empty() {
        return Vec::new();
    }
    let needs_set: HashSet<&str> = job.needs.iter().map(|need| need.job.as_str()).collect();
    job.dependencies
        .iter()
        .map(|dep| {
            if needs_set.contains(dep.as_str()) {
                format!("• {dep} (from needs)")
            } else {
                format!("• {dep}")
            }
        })
        .collect()
}

fn plan_cache_lines(job: &Job) -> Vec<String> {
    if job.cache.is_empty() {
        return Vec::new();
    }
    job.cache.iter().map(format_cache_line).collect()
}

fn format_cache_line(cache: &CacheConfig) -> String {
    let policy = cache_policy_label(cache.policy);
    let paths = if cache.paths.is_empty() {
        "no paths specified".to_string()
    } else {
        cache
            .paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("• key {} ({policy}) – paths: {paths}", cache.key)
}

fn emit_section<F>(display: &DisplayFormatter, title: &str, lines: &[String], emit_line: &mut F)
where
    F: FnMut(String),
{
    if lines.is_empty() {
        return;
    }
    let header_label = format!("    {title}:");
    let header = display.bold_cyan(&header_label);
    emit_line(format!("{header} {}", lines[0]));
    for line in lines.iter().skip(1) {
        emit_line(format!("        {line}"));
    }
}

fn plan_relationship_tree_lines(plan: &JobPlan, planned: &PlannedJob) -> Option<Vec<String>> {
    let tree = build_relationship_tree(plan, planned)?;
    let mut buffer = String::new();
    write_tree(&mut buffer, &tree).ok()?;
    Some(buffer.lines().map(|line| line.to_string()).collect())
}

fn build_relationship_tree(plan: &JobPlan, planned: &PlannedJob) -> Option<Tree> {
    let mut sections = Vec::new();
    let dependency_nodes = dependency_tree_nodes(plan, planned);
    if !dependency_nodes.is_empty() {
        sections.push(Tree::Node("depends on".to_string(), dependency_nodes));
    } else {
        sections.push(Tree::Leaf(vec![
            "depends on previous stage ordering".to_string(),
        ]));
    }

    let need_nodes = need_tree_nodes(plan, planned);
    if !need_nodes.is_empty() {
        sections.push(Tree::Node("needs".to_string(), need_nodes));
    }

    let artifact_nodes = artifact_tree_nodes(&planned.job);
    if !artifact_nodes.is_empty() {
        sections.push(Tree::Node("artifacts".to_string(), artifact_nodes));
    }

    let cache_nodes = cache_tree_nodes(&planned.job);
    if !cache_nodes.is_empty() {
        sections.push(Tree::Node("caches".to_string(), cache_nodes));
    }

    if sections.is_empty() {
        None
    } else {
        Some(Tree::Node(
            format!("{} relationships", planned.job.name),
            sections,
        ))
    }
}

fn dependency_tree_nodes(plan: &JobPlan, planned: &PlannedJob) -> Vec<Tree> {
    planned
        .dependencies
        .iter()
        .map(|dep| build_dependency_tree_node(plan, planned, dep))
        .collect()
}

fn build_dependency_tree_node(plan: &JobPlan, planned: &PlannedJob, dep_name: &str) -> Tree {
    let mut children = Vec::new();
    let need = planned.job.needs.iter().find(|need| need.job == dep_name);
    if let Some(need) = need {
        children.push(tree_leaf(format!(
            "from need ({})",
            describe_need_source(need)
        )));
        children.push(tree_leaf(format!(
            "artifacts requested: {}",
            yes_no(need.needs_artifacts)
        )));
        children.push(tree_leaf(format!("optional: {}", yes_no(need.optional))));
    } else {
        children.push(tree_leaf("from stage order".to_string()));
    }

    let mounts = dependency_mounts(plan, dep_name);
    if !mounts.is_empty() {
        children.push(Tree::Node(
            "mounts".to_string(),
            mounts.into_iter().map(tree_leaf).collect(),
        ));
    }

    Tree::Node(dep_name.to_string(), children)
}

fn dependency_mounts(plan: &JobPlan, dep_name: &str) -> Vec<String> {
    plan.nodes
        .get(dep_name)
        .map(|dep| {
            dep.job
                .artifacts
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn need_tree_nodes(plan: &JobPlan, planned: &PlannedJob) -> Vec<Tree> {
    planned
        .job
        .needs
        .iter()
        .map(|need| build_need_tree_node(plan, planned, need))
        .collect()
}

fn build_need_tree_node(plan: &JobPlan, planned: &PlannedJob, need: &JobDependency) -> Tree {
    let mut children = Vec::new();
    children.push(tree_leaf(format!("source: {}", describe_need_source(need))));
    children.push(tree_leaf(format!(
        "artifacts requested: {}",
        yes_no(need.needs_artifacts)
    )));
    children.push(tree_leaf(format!("optional: {}", yes_no(need.optional))));

    if let Some(filters) = &need.parallel {
        let filter_nodes = filters
            .iter()
            .enumerate()
            .map(|(idx, filter)| {
                if filter.is_empty() {
                    tree_leaf(format!("variant {}", idx + 1))
                } else {
                    let desc = filter
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    tree_leaf(desc)
                }
            })
            .collect();
        children.push(Tree::Node("matrix filters".to_string(), filter_nodes));
    }

    let downloads = planned.dependencies.iter().any(|dep| dep == &need.job);
    children.push(tree_leaf(format!(
        "downloaded via dependencies: {}",
        yes_no(downloads)
    )));
    if downloads && need.needs_artifacts {
        let mounts = dependency_mounts(plan, &need.job);
        if mounts.is_empty() {
            children.push(tree_leaf(
                "mounts available after upstream success".to_string(),
            ));
        } else {
            children.push(Tree::Node(
                "mounts".to_string(),
                mounts.into_iter().map(tree_leaf).collect(),
            ));
        }
    }

    Tree::Node(need.job.clone(), children)
}

fn artifact_tree_nodes(job: &Job) -> Vec<Tree> {
    job.artifacts
        .iter()
        .map(|path| tree_leaf(path.display().to_string()))
        .collect()
}

fn cache_tree_nodes(job: &Job) -> Vec<Tree> {
    job.cache
        .iter()
        .map(|cache| {
            let mut children = vec![tree_leaf(format!(
                "policy: {}",
                cache_policy_label(cache.policy)
            ))];
            if cache.paths.is_empty() {
                children.push(tree_leaf("paths: (none)".to_string()));
            } else {
                children.push(Tree::Node(
                    "paths".to_string(),
                    cache
                        .paths
                        .iter()
                        .map(|path| tree_leaf(path.display().to_string()))
                        .collect(),
                ));
            }
            Tree::Node(format!("key {}", cache.key), children)
        })
        .collect()
}

fn describe_need_source(need: &JobDependency) -> String {
    match &need.source {
        DependencySource::Local => "local".to_string(),
        DependencySource::External(ext) => format!("external {}", ext.project),
    }
}

fn cache_policy_label(policy: CachePolicy) -> &'static str {
    match policy {
        CachePolicy::Pull => "pull",
        CachePolicy::Push => "push",
        CachePolicy::PullPush => "pull-push",
    }
}

fn tree_leaf(text: String) -> Tree {
    Tree::Leaf(vec![text])
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
