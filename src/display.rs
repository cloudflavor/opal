use crate::execution_plan::{ExecutableJob, ExecutionPlan};
use crate::model::{
    CachePolicySpec, CacheSpec, DependencySourceSpec, EnvironmentActionSpec, JobDependencySpec,
    JobSpec,
};
use crate::pipeline::{JobStatus, JobSummary, RuleWhen, VolumeMount};
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

    pub fn format_needs(&self, job: &JobSpec) -> Option<String> {
        if job.needs.is_empty() {
            return None;
        }

        let entries: Vec<String> = job
            .needs
            .iter()
            .map(|need| match &need.source {
                DependencySourceSpec::Local => {
                    if need.needs_artifacts {
                        format!("{} (artifacts)", need.job)
                    } else {
                        need.job.clone()
                    }
                }
                DependencySourceSpec::External(ext) => {
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
    plan: &ExecutionPlan,
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

pub fn print_pipeline_plan<F>(display: &DisplayFormatter, plan: &ExecutionPlan, mut emit_line: F)
where
    F: FnMut(String),
{
    let mut emit = |line: String| {
        if line.is_empty() {
            emit_line(String::new());
        } else {
            emit_line(format!("  {line}"));
        }
    };
    emit(String::new());
    let header = display.bold_blue("pipeline plan");
    emit(header);
    if plan.ordered.is_empty() {
        emit("no jobs scheduled for this context".to_string());
        return;
    }

    let mut current_stage: Option<String> = None;
    for job_name in &plan.ordered {
        let Some(planned) = plan.nodes.get(job_name) else {
            continue;
        };
        if current_stage.as_deref() != Some(planned.instance.stage_name.as_str()) {
            current_stage = Some(planned.instance.stage_name.clone());
            emit(String::new());
            let stage_label = display.bold_cyan("stage:");
            let stage_name = display.bold_magenta(planned.instance.stage_name.as_str());
            emit(format!("{stage_label} {stage_name}"));
        }
        emit_plan_job(display, plan, planned, &mut emit);
    }
}

pub fn collect_pipeline_plan(display: &DisplayFormatter, plan: &ExecutionPlan) -> Vec<String> {
    let mut lines = Vec::new();
    print_pipeline_plan(display, plan, |line| lines.push(line));
    while matches!(lines.first(), Some(existing) if existing.trim().is_empty()) {
        lines.remove(0);
    }
    lines
}

fn emit_plan_job<F>(
    display: &DisplayFormatter,
    plan: &ExecutionPlan,
    planned: &ExecutableJob,
    emit_line: &mut F,
) where
    F: FnMut(String),
{
    let job_label = display.bold_green("  job:");
    let job_name = display.bold_white(planned.instance.job.name.as_str());
    emit_line(format!("{job_label} {job_name}"));

    if let Some(meta) = plan_job_meta(planned) {
        emit_line(format!("{} {}", display.bold_cyan("    info:"), meta));
    }
    if let Some(image) = planned.instance.job.image.as_ref() {
        emit_line(format!(
            "{} {}",
            display.bold_cyan("    image:"),
            format_image_spec(image)
        ));
    }

    emit_section(
        display,
        "depends on",
        &plan_dependency_lines(planned),
        emit_line,
    );
    emit_section(
        display,
        "needs",
        &plan_needs_lines(&planned.instance.job),
        emit_line,
    );
    emit_section(
        display,
        "artifact downloads",
        &plan_dependencies_list(&planned.instance.job),
        emit_line,
    );

    if let Some(artifact_line) = format_artifacts_metadata(display, planned) {
        emit_line(format!(
            "{} {}",
            display.bold_cyan("    artifacts:"),
            artifact_line
        ));
    }
    emit_section(
        display,
        "caches",
        &plan_cache_lines(&planned.instance.job),
        emit_line,
    );
    emit_section(
        display,
        "services",
        &plan_service_lines(&planned.instance.job),
        emit_line,
    );
    emit_section(
        display,
        "tags",
        &plan_tag_lines(&planned.instance.job),
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

    if let Some(timeout) = planned.instance.timeout {
        emit_line(format!(
            "{} {}",
            display.bold_cyan("    timeout:"),
            format_duration(timeout)
        ));
    }

    if let Some(group) = &planned.instance.resource_group {
        emit_line(format!(
            "{} {}",
            display.bold_cyan("    resource group:"),
            group
        ));
    }
}

fn plan_job_meta(planned: &ExecutableJob) -> Option<String> {
    let mut parts = Vec::new();
    parts.push(format!(
        "when {}",
        describe_rule_when(planned.instance.rule.when)
    ));
    if planned.instance.rule.allow_failure {
        parts.push("allow failure".to_string());
    }
    if let Some(delay) = planned.instance.rule.start_in {
        parts.push(format!("start after {}", format_duration(delay)));
    }
    if planned.instance.rule.when == RuleWhen::Manual {
        if planned.instance.rule.manual_auto_run {
            parts.push("auto-run manual".to_string());
        } else {
            parts.push("requires trigger".to_string());
        }
        if let Some(reason) = &planned.instance.rule.manual_reason
            && !reason.is_empty()
        {
            parts.push(format!("reason: {}", reason));
        }
    }
    if planned.instance.job.retry.max > 0 {
        parts.push(format!("retries {}", planned.instance.job.retry.max));
    }
    if planned.instance.job.interruptible {
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

fn format_environment(planned: &ExecutableJob) -> Option<String> {
    let env = planned.instance.job.environment.as_ref()?;
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
    match env.action {
        EnvironmentActionSpec::Start => {}
        EnvironmentActionSpec::Prepare => extra.push("prepare".to_string()),
        EnvironmentActionSpec::Stop => extra.push("stop".to_string()),
        EnvironmentActionSpec::Verify => extra.push("verify".to_string()),
        EnvironmentActionSpec::Access => extra.push("access".to_string()),
    }
    if !extra.is_empty() {
        parts.push(extra.join(", "));
    }
    Some(parts.join(" – "))
}

fn format_artifacts_metadata(
    display: &DisplayFormatter,
    planned: &ExecutableJob,
) -> Option<String> {
    let artifacts = &planned.instance.job.artifacts;
    let mut parts = Vec::new();
    if let Some(name) = &artifacts.name {
        parts.push(format!("name {name}"));
    }
    if let Some(paths) = display.format_paths(&artifacts.paths) {
        parts.push(paths);
    }
    if let Some(expire_in) = artifacts.expire_in {
        parts.push(format!("expire_in {}", format_duration(expire_in)));
    }
    if let Some(dotenv) = &artifacts.report_dotenv {
        parts.push(format!("reports:dotenv {}", dotenv.display()));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" – "))
    }
}

pub fn format_image_spec(image: &crate::model::ImageSpec) -> String {
    let mut extra = Vec::new();
    if let Some(platform) = &image.docker_platform {
        extra.push(format!("platform: {}", platform));
    }
    if let Some(user) = &image.docker_user {
        extra.push(format!("user: {}", user));
    }
    if !image.entrypoint.is_empty() {
        extra.push(format!("entrypoint: [{}]", image.entrypoint.join(", ")));
    }
    if extra.is_empty() {
        image.name.clone()
    } else {
        format!("{} ({})", image.name, extra.join(", "))
    }
}

fn plan_dependency_lines(planned: &ExecutableJob) -> Vec<String> {
    if planned.instance.dependencies.is_empty() {
        return vec!["stage ordering".to_string()];
    }
    let needs_map = planned
        .instance
        .job
        .needs
        .iter()
        .map(|need| (need.job.clone(), need.needs_artifacts))
        .collect::<HashMap<String, bool>>();
    planned
        .instance
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

fn plan_needs_lines(job: &JobSpec) -> Vec<String> {
    if job.needs.is_empty() {
        return Vec::new();
    }
    let dependency_set: HashSet<&str> = job.dependencies.iter().map(|s| s.as_str()).collect();
    job.needs
        .iter()
        .map(|need| format_need_line(need, dependency_set.contains(need.job.as_str())))
        .collect()
}

fn format_need_line(need: &JobDependencySpec, has_dependency: bool) -> String {
    let mut tags = Vec::new();
    match &need.source {
        DependencySourceSpec::Local => tags.push("local".to_string()),
        DependencySourceSpec::External(ext) => tags.push(format!("external {}", ext.project)),
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

fn plan_dependencies_list(job: &JobSpec) -> Vec<String> {
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

fn plan_cache_lines(job: &JobSpec) -> Vec<String> {
    if job.cache.is_empty() {
        return Vec::new();
    }
    job.cache.iter().map(format_cache_line).collect()
}

fn format_cache_line(cache: &CacheSpec) -> String {
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
    format!("• key {} ({policy}) – paths: {paths}", cache.key.describe())
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

fn plan_service_lines(job: &crate::model::JobSpec) -> Vec<String> {
    job.services
        .iter()
        .map(|service| {
            let mut parts = vec![service.image.clone()];
            if !service.aliases.is_empty() {
                parts.push(format!("alias {}", service.aliases.join(",")));
            }
            if !service.entrypoint.is_empty() {
                parts.push(format!("entrypoint [{}]", service.entrypoint.join(", ")));
            }
            if !service.command.is_empty() {
                parts.push(format!("command [{}]", service.command.join(", ")));
            }
            if !service.variables.is_empty() {
                let mut vars = service.variables.keys().cloned().collect::<Vec<_>>();
                vars.sort();
                parts.push(format!("variables {}", vars.join(", ")));
            }
            format!("• {}", parts.join(" – "))
        })
        .collect()
}

fn plan_tag_lines(job: &crate::model::JobSpec) -> Vec<String> {
    if job.tags.is_empty() {
        Vec::new()
    } else {
        vec![format!("• {}", job.tags.join(", "))]
    }
}

fn plan_relationship_tree_lines(
    plan: &ExecutionPlan,
    planned: &ExecutableJob,
) -> Option<Vec<String>> {
    let tree = build_relationship_tree(plan, planned)?;
    let mut buffer = String::new();
    write_tree(&mut buffer, &tree).ok()?;
    Some(buffer.lines().map(|line| line.to_string()).collect())
}

fn build_relationship_tree(plan: &ExecutionPlan, planned: &ExecutableJob) -> Option<Tree> {
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

    let artifact_nodes = artifact_tree_nodes(&planned.instance.job);
    if !artifact_nodes.is_empty() {
        sections.push(Tree::Node("artifacts".to_string(), artifact_nodes));
    }

    let cache_nodes = cache_tree_nodes(&planned.instance.job);
    if !cache_nodes.is_empty() {
        sections.push(Tree::Node("caches".to_string(), cache_nodes));
    }

    if sections.is_empty() {
        None
    } else {
        Some(Tree::Node(
            format!("{} relationships", planned.instance.job.name),
            sections,
        ))
    }
}

fn dependency_tree_nodes(plan: &ExecutionPlan, planned: &ExecutableJob) -> Vec<Tree> {
    planned
        .instance
        .dependencies
        .iter()
        .map(|dep| build_dependency_tree_node(plan, planned, dep))
        .collect()
}

fn build_dependency_tree_node(
    plan: &ExecutionPlan,
    planned: &ExecutableJob,
    dep_name: &str,
) -> Tree {
    let mut children = Vec::new();
    let need = planned
        .instance
        .job
        .needs
        .iter()
        .find(|need| need.job == dep_name);
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

fn dependency_mounts(plan: &ExecutionPlan, dep_name: &str) -> Vec<String> {
    plan.nodes
        .get(dep_name)
        .map(|dep| {
            dep.instance
                .job
                .artifacts
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn need_tree_nodes(plan: &ExecutionPlan, planned: &ExecutableJob) -> Vec<Tree> {
    planned
        .instance
        .job
        .needs
        .iter()
        .map(|need| build_need_tree_node(plan, planned, need))
        .collect()
}

fn build_need_tree_node(
    plan: &ExecutionPlan,
    planned: &ExecutableJob,
    need: &JobDependencySpec,
) -> Tree {
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

    let downloads = planned
        .instance
        .dependencies
        .iter()
        .any(|dep| dep == &need.job);
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

fn artifact_tree_nodes(job: &JobSpec) -> Vec<Tree> {
    job.artifacts
        .paths
        .iter()
        .map(|path| tree_leaf(path.display().to_string()))
        .collect()
}

fn cache_tree_nodes(job: &JobSpec) -> Vec<Tree> {
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
            Tree::Node(format!("key {}", cache.key.describe()), children)
        })
        .collect()
}

fn describe_need_source(need: &JobDependencySpec) -> String {
    match &need.source {
        DependencySourceSpec::Local => "local".to_string(),
        DependencySourceSpec::External(ext) => format!("external {}", ext.project),
    }
}

fn cache_policy_label(policy: CachePolicySpec) -> &'static str {
    match policy {
        CachePolicySpec::Pull => "pull",
        CachePolicySpec::Push => "push",
        CachePolicySpec::PullPush => "pull-push",
    }
}

fn tree_leaf(text: String) -> Tree {
    Tree::Leaf(vec![text])
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
