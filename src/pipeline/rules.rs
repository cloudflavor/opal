use crate::gitlab::Job;
use crate::gitlab::rules::{JobRule, RuleChangesRaw, RuleExistsRaw};
use anyhow::{Context, Result, anyhow};
use globset::{Glob, GlobSetBuilder};
use regex::RegexBuilder;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuleWhen {
    #[default]
    OnSuccess,
    Manual,
    Delayed,
    Never,
    Always,
    OnFailure,
}

impl RuleWhen {
    pub fn requires_success(self) -> bool {
        matches!(self, RuleWhen::OnSuccess | RuleWhen::Delayed)
    }

    pub fn runs_when_failed(self) -> bool {
        matches!(self, RuleWhen::Always | RuleWhen::OnFailure)
    }
}

#[derive(Debug, Default, Clone)]
pub struct RuleEvaluation {
    pub included: bool,
    pub when: RuleWhen,
    pub allow_failure: bool,
    pub start_in: Option<Duration>,
    pub variables: HashMap<String, String>,
    pub manual_auto_run: bool,
    pub manual_reason: Option<String>,
}

impl RuleEvaluation {
    fn default() -> Self {
        Self {
            included: true,
            when: RuleWhen::OnSuccess,
            allow_failure: false,
            start_in: None,
            variables: HashMap::new(),
            manual_auto_run: false,
            manual_reason: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuleContext {
    pub workspace: PathBuf,
    pub env: HashMap<String, String>,
    run_manual: bool,
    default_compare_to: Option<String>,
}

impl RuleContext {
    pub fn new(workspace: &Path) -> Self {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        if !env.contains_key("CI_PIPELINE_SOURCE") {
            env.insert("CI_PIPELINE_SOURCE".into(), "push".into());
        }
        if !env.contains_key("CI_COMMIT_BRANCH")
            && let Ok(branch) = git_current_branch(workspace)
        {
            env.insert("CI_COMMIT_BRANCH".into(), branch);
        }
        if !env.contains_key("CI_DEFAULT_BRANCH")
            && let Ok(branch) = git_default_branch(workspace)
        {
            env.insert("CI_DEFAULT_BRANCH".into(), branch);
        }
        let run_manual = std::env::var("OPAL_RUN_MANUAL").is_ok_and(|v| v == "1");
        let default_compare_to = env.get("CI_DEFAULT_BRANCH").cloned();
        Self {
            workspace: workspace.to_path_buf(),
            env,
            run_manual,
            default_compare_to,
        }
    }

    pub fn env_value(&self, name: &str) -> Option<&str> {
        self.env.get(name).map(|s| s.as_str())
    }

    pub fn var_value(&self, name: &str) -> String {
        self.env
            .get(name)
            .cloned()
            .unwrap_or_else(|| std::env::var(name).unwrap_or_default())
    }

    pub fn pipeline_source(&self) -> &str {
        self.env_value("CI_PIPELINE_SOURCE").unwrap_or("push")
    }

    pub fn compare_reference(&self, override_ref: Option<&str>) -> Option<String> {
        if let Some(raw) = override_ref {
            let expanded = self.expand_variables(raw);
            if expanded.is_empty() {
                None
            } else {
                Some(expanded)
            }
        } else {
            self.inferred_compare_reference()
        }
    }

    pub fn head_reference(&self) -> Option<String> {
        self.env_value("CI_COMMIT_SHA")
            .filter(|sha| !sha.is_empty())
            .map(|sha| sha.to_string())
            .or_else(|| git_head_ref(&self.workspace).ok())
    }

    fn expand_variables(&self, value: &str) -> String {
        let mut output = String::new();
        let chars: Vec<char> = value.chars().collect();
        let mut idx = 0;
        while idx < chars.len() {
            let ch = chars[idx];
            if ch == '$' {
                if idx + 1 < chars.len() && chars[idx + 1] == '{' {
                    let mut end = idx + 2;
                    while end < chars.len() && chars[end] != '}' {
                        end += 1;
                    }
                    if end < chars.len() {
                        let name: String = chars[idx + 2..end].iter().collect();
                        output.push_str(self.env_value(&name).unwrap_or(""));
                        idx = end + 1;
                        continue;
                    }
                } else {
                    let mut end = idx + 1;
                    while end < chars.len()
                        && (chars[end].is_ascii_alphanumeric() || chars[end] == '_')
                    {
                        end += 1;
                    }
                    if end > idx + 1 {
                        let name: String = chars[idx + 1..end].iter().collect();
                        output.push_str(self.env_value(&name).unwrap_or(""));
                        idx = end;
                        continue;
                    }
                }
            }
            output.push(ch);
            idx += 1;
        }
        output
    }

    fn inferred_compare_reference(&self) -> Option<String> {
        let source = self.pipeline_source();
        let inferred = match source {
            "merge_request_event" => self
                .env_value("CI_MERGE_REQUEST_DIFF_BASE_SHA")
                .or_else(|| self.env_value("CI_MERGE_REQUEST_TARGET_BRANCH_SHA"))
                .map(|s| s.to_string())
                .or_else(|| {
                    self.env_value("CI_MERGE_REQUEST_TARGET_BRANCH_NAME")
                        .map(|branch| format!("origin/{branch}"))
                }),
            "push" | "schedule" | "pipeline" | "web" => {
                if let Some(before) = self
                    .env_value("CI_COMMIT_BEFORE_SHA")
                    .filter(|sha| !Self::is_zero_sha(sha))
                    .map(|s| s.to_string())
                {
                    Some(before)
                } else if let Some(default_branch) = &self.default_compare_to {
                    git_merge_base(
                        &self.workspace,
                        default_branch,
                        self.head_reference().as_deref(),
                    )
                    .ok()
                    .flatten()
                } else {
                    None
                }
            }
            _ => None,
        };
        inferred.or_else(|| self.default_compare_to.clone())
    }

    fn is_zero_sha(value: &str) -> bool {
        !value.is_empty() && value.chars().all(|ch| ch == '0')
    }
}

pub fn evaluate_rules(job: &Job, ctx: &RuleContext) -> Result<RuleEvaluation> {
    if job.rules.is_empty() {
        return Ok(RuleEvaluation::default());
    }

    for rule in &job.rules {
        if !rule_matches(rule, ctx)? {
            continue;
        }
        return Ok(apply_rule(rule, ctx));
    }

    Ok(RuleEvaluation {
        included: true,
        ..RuleEvaluation::default()
    })
}

pub fn evaluate_workflow(rules: &[JobRule], ctx: &RuleContext) -> Result<bool> {
    if rules.is_empty() {
        return Ok(true);
    }
    for rule in rules {
        if !rule_matches(rule, ctx)? {
            continue;
        }
        let evaluation = apply_rule(rule, ctx);
        return Ok(evaluation.included);
    }
    Ok(true)
}

fn rule_matches(rule: &JobRule, ctx: &RuleContext) -> Result<bool> {
    if let Some(if_expr) = &rule.if_expr
        && !eval_if_expr(if_expr, ctx)?
    {
        return Ok(false);
    }
    if let Some(changes) = &rule.changes
        && !matches_changes(changes, ctx)?
    {
        return Ok(false);
    }
    if let Some(exists) = &rule.exists
        && !matches_exists(exists, ctx)?
    {
        return Ok(false);
    }
    Ok(true)
}

fn apply_rule(rule: &JobRule, ctx: &RuleContext) -> RuleEvaluation {
    let mut result = RuleEvaluation::default();
    result.variables = rule.variables.clone();
    if let Some(allow) = rule.allow_failure {
        result.allow_failure = allow;
    }
    result.manual_auto_run = ctx.run_manual;

    if let Some(when) = rule.when.as_deref() {
        match when {
            "manual" => {
                result.when = RuleWhen::Manual;
                result.manual_reason = Some("manual job (rules)".into());
            }
            "delayed" => {
                result.when = RuleWhen::Delayed;
                if let Some(start) = rule.start_in.as_deref()
                    && let Some(dur) = parse_duration(start)
                {
                    result.start_in = Some(dur);
                }
            }
            "never" => {
                result.when = RuleWhen::Never;
                result.included = false;
            }
            "always" => {
                result.when = RuleWhen::Always;
            }
            "on_failure" => {
                result.when = RuleWhen::OnFailure;
            }
            _ => {
                result.when = RuleWhen::OnSuccess;
            }
        }
    }

    result
}

fn matches_changes(changes: &RuleChangesRaw, ctx: &RuleContext) -> Result<bool> {
    let paths = changes.paths();
    if paths.is_empty() {
        return Ok(false);
    }
    let compare_ref = ctx.compare_reference(changes.compare_to());
    let head_ref = ctx.head_reference();
    let changed = git_changed_files(&ctx.workspace, compare_ref.as_deref(), head_ref.as_deref())?;
    if changed.is_empty() {
        return Ok(false);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in paths {
        builder.add(Glob::new(pattern).with_context(|| format!("invalid glob '{pattern}'"))?);
    }
    let glob = builder.build()?;
    for path in changed {
        if glob.is_match(&path) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn matches_exists(exists: &RuleExistsRaw, ctx: &RuleContext) -> Result<bool> {
    let paths = exists.paths();
    if paths.is_empty() {
        return Ok(false);
    }
    for pattern in paths {
        let matched = if pattern.contains('*') || pattern.contains('?') {
            let glob = Glob::new(pattern)
                .with_context(|| format!("invalid exists pattern '{pattern}'"))?
                .compile_matcher();
            walk_paths(&ctx.workspace, &glob)?
        } else {
            vec![ctx.workspace.join(pattern)]
        };
        if matched.iter().any(|path| path.exists()) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn walk_paths(root: &Path, matcher: &globset::GlobMatcher) -> Result<Vec<PathBuf>> {
    let mut matches = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_path_buf();
        if matcher.is_match(rel) {
            matches.push(entry.path().to_path_buf());
        }
    }
    Ok(matches)
}

fn git_changed_files(
    workdir: &Path,
    base: Option<&str>,
    head: Option<&str>,
) -> Result<HashSet<String>> {
    let mut cmd = Command::new("git");
    cmd.arg("diff").arg("--name-only");
    match (base, head) {
        (Some(base), Some(head)) => {
            cmd.arg(base);
            cmd.arg(head);
        }
        (Some(base), None) => {
            cmd.arg(base);
            cmd.arg("HEAD");
        }
        (None, Some(head)) => {
            cmd.arg(head);
        }
        (None, None) => {
            cmd.arg("HEAD~1");
            cmd.arg("HEAD");
        }
    }
    cmd.current_dir(workdir);
    let output = cmd.output().context("failed to run git diff")?;
    if !output.status.success() {
        return Ok(HashSet::new());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut set = HashSet::new();
    for line in stdout.lines() {
        if !line.trim().is_empty() {
            set.insert(line.trim().to_string());
        }
    }
    Ok(set)
}

fn git_current_branch(workdir: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .current_dir(workdir)
        .output()
        .context("failed to detect current branch")?;
    if !output.status.success() {
        return Err(anyhow!("git rev-parse returned non-zero"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_head_ref(workdir: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(workdir)
        .output()
        .context("failed to detect HEAD reference")?;
    if !output.status.success() {
        return Err(anyhow!("git rev-parse returned non-zero"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_merge_base(workdir: &Path, base: &str, head: Option<&str>) -> Result<Option<String>> {
    let mut cmd = Command::new("git");
    cmd.arg("merge-base").arg(base);
    if let Some(head) = head {
        cmd.arg(head);
    } else {
        cmd.arg("HEAD");
    }
    cmd.current_dir(workdir);
    let output = cmd.output().context("failed to run git merge-base")?;
    if !output.status.success() {
        return Ok(None);
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        Ok(None)
    } else {
        Ok(Some(sha))
    }
}

fn git_default_branch(workdir: &Path) -> Result<String> {
    let output = Command::new("git")
        .arg("symbolic-ref")
        .arg("refs/remotes/origin/HEAD")
        .current_dir(workdir)
        .output()
        .context("failed to detect default branch")?;
    if !output.status.success() {
        return Err(anyhow!("git symbolic-ref returned non-zero"));
    }
    let ref_name = String::from_utf8_lossy(&output.stdout);
    if let Some(pos) = ref_name.rfind('/') {
        Ok(ref_name[pos + 1..].trim().to_string())
    } else {
        Ok("main".into())
    }
}

fn parse_duration(value: &str) -> Option<Duration> {
    humantime::Duration::from_str(value).map(|d| d.into()).ok()
}

fn eval_if_expr(expr: &str, ctx: &RuleContext) -> Result<bool> {
    let mut parser = ExprParser::new(expr, ctx);
    let value = parser.parse_expression()?;
    Ok(value)
}

struct ExprParser {
    tokens: Vec<Token>,
    pos: usize,
}

impl ExprParser {
    fn new(input: &str, ctx: &RuleContext) -> Self {
        let tokens = tokenize(input, ctx);
        Self { tokens, pos: 0 }
    }

    fn parse_expression(&mut self) -> Result<bool> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<bool> {
        let mut value = self.parse_and()?;
        while self.matches(TokenKind::Or) {
            value = value || self.parse_and()?;
        }
        Ok(value)
    }

    fn parse_and(&mut self) -> Result<bool> {
        let mut value = self.parse_not()?;
        while self.matches(TokenKind::And) {
            value = value && self.parse_not()?;
        }
        Ok(value)
    }

    fn parse_not(&mut self) -> Result<bool> {
        if self.matches(TokenKind::Not) {
            return Ok(!self.parse_not()?);
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<bool> {
        if self.matches(TokenKind::LParen) {
            let value = self.parse_expression()?;
            self.consume(TokenKind::RParen)?;
            return Ok(value);
        }
        let left = self.parse_operand()?;
        if let Some(op) = self.peek_operator() {
            self.advance();
            let right = self.parse_operand()?;
            return self.evaluate_comparator(op, left, right);
        }
        Ok(!left.is_empty())
    }

    fn evaluate_comparator(&self, op: TokenKind, left: String, right: String) -> Result<bool> {
        match op {
            TokenKind::Eq => Ok(left == right),
            TokenKind::Ne => Ok(left != right),
            TokenKind::RegexEq => Ok(match_regex(&left, &right)?),
            TokenKind::RegexNe => Ok(!match_regex(&left, &right)?),
            _ => Err(anyhow!("unsupported comparator")),
        }
    }

    fn parse_operand(&mut self) -> Result<String> {
        if self.matches(TokenKind::Variable) {
            return Ok(self.last_token_value().unwrap_or_default());
        }
        if self.matches(TokenKind::Literal) {
            return Ok(self.last_token_value().unwrap_or_default());
        }
        Err(anyhow!("expected operand"))
    }

    fn matches(&mut self, kind: TokenKind) -> bool {
        if self.check(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn check(&self, kind: TokenKind) -> bool {
        self.tokens
            .get(self.pos)
            .map(|t| t.kind == kind)
            .unwrap_or(false)
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn consume(&mut self, kind: TokenKind) -> Result<()> {
        if self.check(kind) {
            self.advance();
            Ok(())
        } else {
            Err(anyhow!("expected token"))
        }
    }

    fn peek_operator(&self) -> Option<TokenKind> {
        self.tokens.get(self.pos).and_then(|t| match t.kind {
            TokenKind::Eq | TokenKind::Ne | TokenKind::RegexEq | TokenKind::RegexNe => Some(t.kind),
            _ => None,
        })
    }

    fn last_token_value(&self) -> Option<String> {
        self.tokens.get(self.pos - 1).and_then(|t| t.value.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TokenKind {
    And,
    Or,
    Not,
    LParen,
    RParen,
    Eq,
    Ne,
    RegexEq,
    RegexNe,
    Variable,
    Literal,
}

#[derive(Debug, Clone)]
struct Token {
    kind: TokenKind,
    value: Option<String>,
}

fn tokenize(input: &str, ctx: &RuleContext) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut idx = 0;
    while idx < chars.len() {
        match chars[idx] {
            ' ' | '\t' | '\n' => idx += 1,
            '&' if idx + 1 < chars.len() && chars[idx + 1] == '&' => {
                tokens.push(Token {
                    kind: TokenKind::And,
                    value: None,
                });
                idx += 2;
            }
            '|' if idx + 1 < chars.len() && chars[idx + 1] == '|' => {
                tokens.push(Token {
                    kind: TokenKind::Or,
                    value: None,
                });
                idx += 2;
            }
            '!' if idx + 1 < chars.len() && chars[idx + 1] == '=' => {
                tokens.push(Token {
                    kind: TokenKind::Ne,
                    value: None,
                });
                idx += 2;
            }
            '=' if idx + 1 < chars.len() && chars[idx + 1] == '=' => {
                tokens.push(Token {
                    kind: TokenKind::Eq,
                    value: None,
                });
                idx += 2;
            }
            '=' if idx + 1 < chars.len() && chars[idx + 1] == '~' => {
                tokens.push(Token {
                    kind: TokenKind::RegexEq,
                    value: None,
                });
                idx += 2;
            }
            '!' if idx + 1 < chars.len() && chars[idx + 1] == '~' => {
                tokens.push(Token {
                    kind: TokenKind::RegexNe,
                    value: None,
                });
                idx += 2;
            }
            '!' => {
                tokens.push(Token {
                    kind: TokenKind::Not,
                    value: None,
                });
                idx += 1;
            }
            '(' => {
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    value: None,
                });
                idx += 1;
            }
            ')' => {
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    value: None,
                });
                idx += 1;
            }
            '$' => {
                let start = idx + 1;
                idx = start;
                while idx < chars.len()
                    && (chars[idx].is_ascii_alphanumeric()
                        || chars[idx] == '_'
                        || chars[idx] == ':')
                {
                    idx += 1;
                }
                let name = input[start..idx].to_string();
                let value = ctx.var_value(&name);
                tokens.push(Token {
                    kind: TokenKind::Variable,
                    value: Some(value),
                });
            }
            '\'' | '"' => {
                let quote = chars[idx];
                idx += 1;
                let start = idx;
                while idx < chars.len() && chars[idx] != quote {
                    idx += 1;
                }
                let value = input[start..idx].to_string();
                idx += 1;
                tokens.push(Token {
                    kind: TokenKind::Literal,
                    value: Some(value),
                });
            }
            _ => {
                let start = idx;
                while idx < chars.len()
                    && !chars[idx].is_whitespace()
                    && !matches!(chars[idx], '(' | ')' | '&' | '|' | '=' | '!')
                {
                    idx += 1;
                }
                let value = input[start..idx].to_string();
                tokens.push(Token {
                    kind: TokenKind::Literal,
                    value: Some(value),
                });
            }
        }
    }
    tokens
}

fn match_regex(value: &str, pattern: &str) -> Result<bool> {
    let (body, flags) = if let Some(stripped) = pattern.strip_prefix('/') {
        if let Some(end) = stripped.rfind('/') {
            let body = &stripped[..end];
            let flag = &stripped[end + 1..];
            (body.to_string(), flag.to_string())
        } else {
            (pattern.to_string(), String::new())
        }
    } else {
        (pattern.to_string(), String::new())
    };
    let mut builder = RegexBuilder::new(&body);
    if flags.contains('i') {
        builder.case_insensitive(true);
    }
    let regex = builder.build()?;
    Ok(regex.is_match(value))
}
