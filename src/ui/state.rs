use super::types::{
    CURRENT_HISTORY_KEY, HistoryAction, LOG_SCROLL_HALF, LOG_SCROLL_PAGE, LOG_SCROLL_STEP,
    PaneFocus, UiJobInfo, UiJobResources, UiJobStatus,
};
use crate::history::{HistoryEntry, HistoryJob, HistoryStatus};
use anyhow::{Context, Result, anyhow};
use include_dir::{Dir, include_dir};
use owo_colors::OwoColorize;
use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;
use walkdir::WalkDir;

static EMBEDDED_DOCS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/docs");

#[derive(Clone)]
struct HelpDocument {
    title: String,
    path: PathBuf,
    lines: Vec<Line<'static>>,
}

#[derive(Clone, Copy)]
enum HelpView {
    Shortcuts,
    Document(usize),
}

pub(super) struct UiState {
    jobs: Vec<UiJobState>,
    order: HashMap<String, usize>,
    selected: usize,
    history: Vec<HistoryEntry>,
    current_run_id: String,
    focus: PaneFocus,
    history_selection: usize,
    history_scroll: usize,
    history_collapsed: HashMap<String, bool>,
    history_preview: Option<HistoryPreview>,
    history_view: Option<HistoryRunView>,
    show_help: bool,
    help_view: HelpView,
    help_scroll: u16,
    help_viewport: u16,
    help_width: u16,
    help_docs: Vec<HelpDocument>,
    job_resources: HashMap<String, UiJobResources>,
    plan_text: String,
    workdir: PathBuf,
    history_height: u16,
}

impl UiState {
    pub(super) fn new(
        jobs: Vec<UiJobInfo>,
        history: Vec<HistoryEntry>,
        current_run_id: String,
        job_resources: HashMap<String, UiJobResources>,
        plan_text: String,
        workdir: PathBuf,
    ) -> Self {
        let mut order = HashMap::new();
        let job_states: Vec<UiJobState> = jobs
            .into_iter()
            .enumerate()
            .map(|(idx, job)| {
                order.insert(job.name.clone(), idx);
                UiJobState::from(job)
            })
            .collect();

        let mut history_collapsed = HashMap::new();
        history_collapsed.insert(CURRENT_HISTORY_KEY.to_string(), false);
        for entry in &history {
            history_collapsed.insert(entry.run_id.clone(), true);
        }

        Self {
            jobs: job_states,
            order,
            selected: 0,
            history,
            current_run_id,
            focus: PaneFocus::Jobs,
            history_selection: 0,
            history_scroll: 0,
            history_collapsed,
            history_preview: None,
            history_view: None,
            show_help: false,
            help_view: HelpView::Shortcuts,
            help_scroll: 0,
            help_viewport: 1,
            help_width: 1,
            help_docs: HelpDocument::discover(),
            job_resources,
            plan_text,
            workdir,
            history_height: 0,
        }
    }

    pub(super) fn active_jobs(&self) -> &[UiJobState] {
        if let Some(view) = &self.history_view {
            &view.jobs
        } else {
            &self.jobs
        }
    }

    pub(super) fn active_selected_index(&self) -> usize {
        if let Some(view) = &self.history_view {
            view.selected
        } else {
            self.selected
        }
    }

    pub(super) fn set_active_selected_index(&mut self, idx: usize) {
        if let Some(view) = &mut self.history_view {
            if idx < view.jobs.len() {
                view.selected = idx;
            }
        } else if idx < self.jobs.len() {
            self.selected = idx;
        }
        self.on_active_selection_changed();
    }

    pub(super) fn active_job(&self) -> Option<&UiJobState> {
        self.active_jobs().get(self.active_selected_index())
    }

    pub(super) fn active_job_mut(&mut self) -> Option<&mut UiJobState> {
        let idx = self.active_selected_index();
        if let Some(view) = &mut self.history_view {
            view.jobs.get_mut(idx)
        } else {
            self.jobs.get_mut(idx)
        }
    }

    pub(super) fn tabs(&self, width: u16) -> (Paragraph<'static>, u16) {
        let (lines, rows) = self.tab_lines(width as usize);
        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Jobs"))
            .wrap(Wrap { trim: false });

        let content_height = rows.saturating_add(2); // account for top/bottom borders

        (paragraph, content_height)
    }

    pub(super) fn history_widget(&mut self, height: u16) -> Paragraph<'static> {
        let nodes = self.history_nodes();
        if nodes.is_empty() {
            self.history_selection = 0;
            self.history_scroll = 0;
            self.clear_history_preview();
            return Paragraph::new(vec![Line::from("no runs recorded")])
                .block(Block::default().borders(Borders::ALL).title("Runs"))
                .wrap(Wrap { trim: false });
        }

        self.history_height = height;
        self.clamp_and_scroll_history(nodes.len());

        let viewport = Self::history_viewport(height);
        let visible = if viewport == 0 {
            &nodes[..]
        } else {
            let end = (self.history_scroll + viewport).min(nodes.len());
            &nodes[self.history_scroll..end]
        };

        let lines: Vec<Line<'static>> = visible
            .iter()
            .enumerate()
            .map(|(offset, node)| {
                let idx = self.history_scroll + offset;
                let line = node.line.clone();
                if self.focus == PaneFocus::History && idx == self.history_selection {
                    Self::apply_history_highlight(line)
                } else {
                    line
                }
            })
            .collect();

        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Runs"))
            .wrap(Wrap { trim: false })
    }

    pub(super) fn history_status_style(status: HistoryStatus) -> Style {
        match status {
            HistoryStatus::Success => Style::default().fg(Color::Green),
            HistoryStatus::Failed => Style::default().fg(Color::Red),
            HistoryStatus::Skipped => Style::default().fg(Color::Yellow),
            HistoryStatus::Running => Style::default().fg(Color::Cyan),
        }
    }

    pub(super) fn history_preview_view(&self, width: u16, height: u16) -> Paragraph<'static> {
        let Some(preview) = &self.history_preview else {
            return Paragraph::new(vec![Line::from("no log loaded")])
                .block(Block::default().borders(Borders::ALL).title("Logs"))
                .wrap(Wrap { trim: false });
        };

        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            format!("History log: {}", preview.title),
            Style::default().fg(Color::Cyan),
        )));
        lines.push(Line::from(Span::styled(
            format!("Source: {}", preview.path.display()),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::raw(" ")));

        let inner_height = height.saturating_sub(3);
        let inner_width = width.saturating_sub(2).max(1) as usize;
        lines.extend(preview.visible_lines(inner_width, inner_height as usize));

        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Logs"))
            .wrap(Wrap { trim: false })
    }

    pub(super) fn scroll_history_preview_up(&mut self, lines: usize) {
        if let Some(preview) = self.history_preview.as_mut() {
            preview.scroll_lines_up(lines);
        }
    }

    pub(super) fn scroll_history_preview_down(&mut self, lines: usize) {
        if let Some(preview) = self.history_preview.as_mut() {
            preview.scroll_lines_down(lines);
        }
    }

    pub(super) fn scroll_history_preview_to_top(&mut self) {
        if let Some(preview) = self.history_preview.as_mut() {
            preview.scroll_to_top();
        }
    }

    pub(super) fn scroll_history_preview_to_bottom(&mut self) {
        if let Some(preview) = self.history_preview.as_mut() {
            preview.scroll_to_bottom();
        }
    }

    pub(super) fn history_nodes(&self) -> Vec<HistoryRenderNode> {
        let mut nodes = Vec::new();

        let current_collapsed = self.is_run_collapsed(CURRENT_HISTORY_KEY);
        let mut header_spans = Vec::new();
        header_spans.push(Span::styled(
            format!("{} ", if current_collapsed { "▸" } else { "▾" }),
            Style::default().fg(Color::DarkGray),
        ));
        header_spans.push(Span::styled(
            format!("{} (active)", self.current_run_id),
            Self::history_status_style(self.current_run_status()),
        ));
        if self.history_view.is_none() {
            header_spans.push(Span::styled(
                " [viewing]".to_string(),
                Style::default().fg(Color::Yellow),
            ));
        }
        nodes.push(HistoryRenderNode {
            key: HistoryNodeKey::CurrentRun,
            parent_index: None,
            line: Line::from(header_spans),
        });
        let current_header_idx = nodes.len() - 1;
        if !current_collapsed {
            let total = self.jobs.len();
            for (idx, job) in self.jobs.iter().enumerate() {
                let connector = if idx + 1 == total { "└─" } else { "├─" };
                nodes.push(HistoryRenderNode {
                    key: HistoryNodeKey::CurrentJob(idx),
                    parent_index: Some(current_header_idx),
                    line: Self::history_job_line(
                        connector,
                        &job.name,
                        &job.stage,
                        &job.log_hash,
                        Self::history_status_from_ui(job.status),
                    ),
                });
                let job_idx = nodes.len() - 1;
                if let Some(resources) = self.job_resources.get(&job.name) {
                    self.append_job_resource_nodes(&mut nodes, job_idx, resources);
                }
            }
        }

        for entry in self.history.iter().rev() {
            let collapsed = self.is_run_collapsed(&entry.run_id);
            let mut spans = Vec::new();
            spans.push(Span::styled(
                format!("{} ", if collapsed { "▸" } else { "▾" }),
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::styled(
                entry.run_id.clone(),
                Self::history_status_style(entry.status),
            ));
            spans.push(Span::styled(
                format!(" ({})", entry.finished_at),
                Style::default().fg(Color::DarkGray),
            ));
            if self
                .history_view
                .as_ref()
                .map(|view| view.run_id == entry.run_id)
                .unwrap_or(false)
            {
                spans.push(Span::styled(
                    " [viewing]".to_string(),
                    Style::default().fg(Color::Yellow),
                ));
            }
            nodes.push(HistoryRenderNode {
                key: HistoryNodeKey::FinishedRun {
                    run_id: entry.run_id.clone(),
                },
                parent_index: None,
                line: Line::from(spans),
            });
            let header_idx = nodes.len() - 1;
            if collapsed {
                continue;
            }
            let total = entry.jobs.len();
            for (idx, job) in entry.jobs.iter().enumerate() {
                let connector = if idx + 1 == total { "└─" } else { "├─" };
                let log_path = job.log_path.as_ref().map(PathBuf::from).or_else(|| {
                    Some(PathBuf::from(format!(
                        ".opal/logs/{}/{}.log",
                        entry.run_id, job.log_hash
                    )))
                });
                nodes.push(HistoryRenderNode {
                    key: HistoryNodeKey::FinishedJob {
                        run_id: entry.run_id.clone(),
                        job_name: job.name.clone(),
                        log_path,
                    },
                    parent_index: Some(header_idx),
                    line: Self::history_job_line(
                        connector,
                        &job.name,
                        &job.stage,
                        &job.log_hash,
                        job.status,
                    ),
                });
                let job_idx = nodes.len() - 1;
                let resources = UiJobResources::from(job);
                self.append_job_resource_nodes(&mut nodes, job_idx, &resources);
            }
        }

        nodes
    }

    fn append_job_resource_nodes(
        &self,
        nodes: &mut Vec<HistoryRenderNode>,
        parent_index: usize,
        resources: &UiJobResources,
    ) {
        let mut entries = Vec::new();
        if let Some(dir) = &resources.artifact_dir {
            entries.push(ResourceEntry::dir(
                format!("Artifact dir: {}", self.relative_display(dir)),
                PathBuf::from(dir),
            ));
        }
        if !resources.artifact_paths.is_empty() {
            let display_paths: Vec<String> = resources
                .artifact_paths
                .iter()
                .map(|p| self.relative_display(p))
                .collect();
            let summary = Self::summarize_list(&display_paths);
            entries.push(ResourceEntry::info(format!("Artifacts: {summary}")));
        }
        if !resources.caches.is_empty() {
            let cache_summary = resources
                .caches
                .iter()
                .map(|cache| {
                    format!(
                        "{} ({}) @ {}",
                        cache.key,
                        cache.policy,
                        self.relative_display(&cache.host)
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            entries.push(ResourceEntry::info(format!("Caches: {cache_summary}")));
        }
        if entries.is_empty() {
            return;
        }
        let total = entries.len();
        for (idx, entry) in entries.into_iter().enumerate() {
            let connector = if idx + 1 == total { "└─" } else { "├─" };
            let line = Self::resource_line(connector, &entry.label, entry.color);
            nodes.push(HistoryRenderNode {
                key: entry.key,
                parent_index: Some(parent_index),
                line,
            });
        }
    }

    pub(super) fn history_job_line(
        connector: &str,
        name: &str,
        stage: &str,
        hash: &str,
        status: HistoryStatus,
    ) -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!("  {} ", connector),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(name.to_string(), Self::history_status_style(status)),
            Span::styled(
                format!(" [{}]", stage),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(format!(" {}", hash), Style::default().fg(Color::DarkGray)),
        ])
    }

    pub(super) fn history_status_from_ui(status: UiJobStatus) -> HistoryStatus {
        match status {
            UiJobStatus::Success => HistoryStatus::Success,
            UiJobStatus::Failed => HistoryStatus::Failed,
            UiJobStatus::Skipped => HistoryStatus::Skipped,
            UiJobStatus::Running | UiJobStatus::Pending => HistoryStatus::Running,
        }
    }

    pub(super) fn current_run_status(&self) -> HistoryStatus {
        if self
            .jobs
            .iter()
            .any(|job| job.status == UiJobStatus::Failed)
        {
            HistoryStatus::Failed
        } else if self
            .jobs
            .iter()
            .any(|job| matches!(job.status, UiJobStatus::Running | UiJobStatus::Pending))
        {
            HistoryStatus::Running
        } else if self
            .jobs
            .iter()
            .all(|job| job.status == UiJobStatus::Skipped)
        {
            HistoryStatus::Skipped
        } else {
            HistoryStatus::Success
        }
    }

    pub(super) fn apply_history_highlight(mut line: Line<'static>) -> Line<'static> {
        let highlight = Style::default()
            .bg(Color::DarkGray)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        for span in &mut line.spans {
            span.style = span.style.patch(highlight);
        }
        line
    }

    pub(super) fn is_run_collapsed(&self, key: &str) -> bool {
        if key == CURRENT_HISTORY_KEY {
            self.history_collapsed.get(key).copied().unwrap_or(false)
        } else {
            self.history_collapsed.get(key).copied().unwrap_or(true)
        }
    }

    pub(super) fn set_run_collapsed(&mut self, key: &str, collapsed: bool) {
        self.history_collapsed.insert(key.to_string(), collapsed);
    }

    pub(super) fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            PaneFocus::Jobs => PaneFocus::History,
            PaneFocus::History => PaneFocus::Jobs,
        };
        if self.focus_is_history() {
            self.history_selection = self
                .history_selection
                .min(self.history_nodes().len().saturating_sub(1));
        }
    }

    pub(super) fn focus_is_history(&self) -> bool {
        matches!(self.focus, PaneFocus::History)
    }

    pub(super) fn push_history_entry(&mut self, entry: HistoryEntry) {
        self.history_collapsed
            .entry(entry.run_id.clone())
            .or_insert(true);
        self.history.push(entry);
    }

    pub(super) fn view_history_run(&mut self, run_id: &str) -> Result<()> {
        if run_id == self.current_run_id {
            self.close_history_view();
            return Ok(());
        }
        let entry = self
            .history
            .iter()
            .find(|entry| entry.run_id == run_id)
            .cloned()
            .ok_or_else(|| anyhow!("history for run '{run_id}' not found"))?;
        let jobs: Vec<UiJobState> = entry
            .jobs
            .iter()
            .map(|job| UiJobState::from_history(run_id, job))
            .collect();
        self.history_view = Some(HistoryRunView {
            run_id: run_id.to_string(),
            jobs,
            selected: 0,
        });
        self.focus = PaneFocus::Jobs;
        self.on_active_selection_changed();
        Ok(())
    }

    pub(super) fn close_history_view(&mut self) {
        self.history_view = None;
        self.history_preview = None;
        self.focus = PaneFocus::Jobs;
        if self.selected >= self.jobs.len() && !self.jobs.is_empty() {
            self.selected = self.jobs.len() - 1;
        }
        self.on_active_selection_changed();
    }

    pub(super) fn history_move_up(&mut self) {
        let nodes = self.history_nodes();
        if nodes.is_empty() {
            self.history_selection = 0;
            return;
        }
        self.clear_history_preview();
        if self.history_selection >= nodes.len() {
            self.history_selection = nodes.len().saturating_sub(1);
        } else if self.history_selection > 0 {
            self.history_selection -= 1;
        }
        self.clamp_and_scroll_history(nodes.len());
    }

    pub(super) fn history_move_down(&mut self) {
        let nodes = self.history_nodes();
        if nodes.is_empty() {
            self.history_selection = 0;
            return;
        }
        self.clear_history_preview();
        if self.history_selection >= nodes.len() {
            self.history_selection = nodes.len().saturating_sub(1);
        } else if self.history_selection + 1 < nodes.len() {
            self.history_selection += 1;
        }
        self.clamp_and_scroll_history(nodes.len());
    }

    pub(super) fn history_move_home(&mut self) {
        self.clear_history_preview();
        self.history_selection = 0;
        self.history_scroll = 0;
    }

    pub(super) fn history_move_end(&mut self) {
        let len = self.history_nodes().len();
        if len == 0 {
            self.history_selection = 0;
            self.history_scroll = 0;
        } else {
            self.history_selection = len - 1;
        }
        self.clear_history_preview();
        self.clamp_and_scroll_history(len);
    }

    pub(super) fn history_move_left(&mut self) {
        let nodes = self.history_nodes();
        if nodes.is_empty() {
            self.history_selection = 0;
            return;
        }
        self.clear_history_preview();
        let idx = self.history_selection.min(nodes.len() - 1);
        match &nodes[idx].key {
            HistoryNodeKey::CurrentRun => {
                if !self.is_run_collapsed(CURRENT_HISTORY_KEY) {
                    self.set_run_collapsed(CURRENT_HISTORY_KEY, true);
                }
            }
            HistoryNodeKey::FinishedRun { run_id } => {
                if !self.is_run_collapsed(run_id) {
                    self.set_run_collapsed(run_id, true);
                }
            }
            _ => {
                if let Some(parent) = nodes[idx].parent_index {
                    self.history_selection = parent;
                }
            }
        }
        self.refresh_history_bounds();
    }

    pub(super) fn history_move_right(&mut self) {
        let nodes = self.history_nodes();
        if nodes.is_empty() {
            self.history_selection = 0;
            return;
        }
        self.clear_history_preview();
        let idx = self.history_selection.min(nodes.len() - 1);
        match &nodes[idx].key {
            HistoryNodeKey::CurrentRun => {
                if self.is_run_collapsed(CURRENT_HISTORY_KEY) {
                    self.set_run_collapsed(CURRENT_HISTORY_KEY, false);
                } else if let Some(next) = nodes.get(idx + 1)
                    && next.parent_index == Some(idx)
                {
                    self.history_selection = idx + 1;
                }
            }
            HistoryNodeKey::FinishedRun { run_id } => {
                if self.is_run_collapsed(run_id) {
                    self.set_run_collapsed(run_id, false);
                } else if let Some(next) = nodes.get(idx + 1)
                    && next.parent_index == Some(idx)
                {
                    self.history_selection = idx + 1;
                }
            }
            _ => {}
        }
        self.refresh_history_bounds();
    }

    pub(super) fn history_activate(&mut self) -> Option<HistoryAction> {
        let nodes = self.history_nodes();
        if nodes.is_empty() {
            return None;
        }
        let idx = self.history_selection.min(nodes.len() - 1);
        match &nodes[idx].key {
            HistoryNodeKey::CurrentRun => {
                self.close_history_view();
                None
            }
            HistoryNodeKey::FinishedRun { run_id } => Some(HistoryAction::ViewRun(run_id.clone())),
            HistoryNodeKey::CurrentJob(idx) => Some(HistoryAction::SelectJob(*idx)),
            HistoryNodeKey::FinishedJob {
                run_id,
                job_name,
                log_path,
            } => log_path.clone().map(|path| HistoryAction::ViewLog {
                title: format!("{run_id} • {job_name}"),
                path,
            }),
            HistoryNodeKey::ResourceDir { title, path } => Some(HistoryAction::ViewDir {
                title: title.clone(),
                path: path.clone(),
            }),
            HistoryNodeKey::ResourceInfo => None,
        }
    }

    pub(super) fn clear_history_preview(&mut self) {
        self.history_preview = None;
    }

    pub(super) fn load_history_preview(&mut self, title: String, path: &Path) -> Result<()> {
        self.history_preview = None;
        let file =
            File::open(path).with_context(|| format!("failed to open log {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        for line in reader.lines() {
            lines.push(line.with_context(|| format!("failed to read log {}", path.display()))?);
        }
        self.history_preview = Some(HistoryPreview {
            title,
            path: path.to_path_buf(),
            lines,
            scroll_offset: 0,
        });
        self.focus = PaneFocus::Jobs;
        Ok(())
    }

    pub(super) fn set_history_preview_message(
        &mut self,
        title: String,
        path: &Path,
        message: String,
    ) {
        self.history_preview = Some(HistoryPreview {
            title,
            path: path.to_path_buf(),
            lines: vec![message],
            scroll_offset: 0,
        });
        self.focus = PaneFocus::Jobs;
    }

    pub(super) fn load_directory_preview(&mut self, title: String, path: &Path) -> Result<()> {
        self.history_preview = None;
        if !path.exists() {
            self.set_history_preview_message(
                title,
                path,
                format!("directory {} not found", path.display()),
            );
            return Ok(());
        }
        let mut lines = Vec::new();
        lines.push(format!("Directory: {}", path.display()));
        let mut count = 0usize;
        let max_entries = 600;
        let max_depth = 5;
        for entry in WalkDir::new(path).max_depth(max_depth).sort_by_file_name() {
            let entry = entry.with_context(|| "failed to read directory entry")?;
            let depth = entry.depth();
            let indent = "  ".repeat(depth);
            let name = entry.file_name().to_string_lossy();
            let marker = if entry.file_type().is_dir() {
                "[d]"
            } else {
                "[f]"
            };
            lines.push(format!("{indent}{marker} {name}"));
            count += 1;
            if count >= max_entries {
                lines.push(format!(
                    "... truncated to {max_entries} entries (use pager to inspect directly)"
                ));
                break;
            }
        }
        self.history_preview = Some(HistoryPreview {
            title,
            path: path.to_path_buf(),
            lines,
            scroll_offset: 0,
        });
        self.focus = PaneFocus::Jobs;
        Ok(())
    }

    pub(super) fn on_active_selection_changed(&mut self) {
        if let Some(view) = &self.history_view {
            if let Some(job) = view.jobs.get(view.selected) {
                let path = job.log_path.clone();
                let title = format!("{} • {}", view.run_id, job.name);
                if let Err(err) = self.load_history_preview(title.clone(), &path) {
                    self.set_history_preview_message(
                        title,
                        &path,
                        format!("failed to load log: {err}"),
                    );
                }
            } else {
                self.history_preview = None;
            }
        } else {
            self.history_preview = None;
            if let Some(job) = self.jobs.get_mut(self.selected) {
                job.auto_follow();
            }
        }
    }

    pub(super) fn clamp_history_selection(&mut self, len: usize) {
        if len == 0 {
            self.history_selection = 0;
        } else if self.history_selection >= len {
            self.history_selection = len - 1;
        }
    }

    pub(super) fn ensure_history_visible(&mut self, height: u16, len: usize) {
        let viewport = Self::history_viewport(height);
        if viewport == 0 || len == 0 {
            self.history_scroll = 0;
            return;
        }
        if self.history_scroll + viewport > len {
            self.history_scroll = len.saturating_sub(viewport);
        }
        if self.history_selection < self.history_scroll {
            self.history_scroll = self.history_selection;
        } else if self.history_selection >= self.history_scroll + viewport {
            self.history_scroll = self.history_selection + 1 - viewport;
        }
    }

    pub(super) fn history_viewport(height: u16) -> usize {
        usize::from(height.saturating_sub(2).max(1))
    }

    fn history_pane_height(&self) -> u16 {
        self.history_height.max(1)
    }

    fn help_text_width(&self) -> u16 {
        self.help_width.max(1)
    }

    fn clamp_and_scroll_history(&mut self, len: usize) {
        self.clamp_history_selection(len);
        self.ensure_history_visible(self.history_pane_height(), len);
    }

    fn refresh_history_bounds(&mut self) {
        let len = self.history_nodes().len();
        self.clamp_and_scroll_history(len);
    }

    pub(super) fn tab_lines(&self, available: usize) -> (Vec<Line<'static>>, u16) {
        let jobs = self.active_jobs();
        if jobs.is_empty() {
            return (vec![Line::raw("")], 1);
        }

        let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut width = 0usize;

        for (idx, job) in jobs.iter().enumerate() {
            let label_spans = self.build_label_spans(job, idx == self.active_selected_index());
            let label_width = Line::from(label_spans.clone()).width();
            let separator_width = if current.is_empty() { 0 } else { 3 };

            if !current.is_empty() && width + separator_width + label_width > available {
                rows.push(current);
                current = Vec::new();
                width = 0;
            }

            if !current.is_empty() {
                current.push(Span::raw(" │ ".to_string()));
                width += 3;
            }

            width += label_width;
            current.extend(label_spans);
        }

        if !current.is_empty() {
            rows.push(current);
        }

        if rows.is_empty() {
            rows.push(Vec::new());
        }

        let row_count: u16 = rows.len().try_into().unwrap_or(u16::MAX);
        let lines = rows.into_iter().map(Line::from).collect();

        (lines, row_count)
    }

    pub(super) fn build_label_spans(&self, job: &UiJobState, selected: bool) -> Vec<Span<'static>> {
        let (icon_char, icon_color) = job.status.icon();
        let highlight = if selected {
            Some(
                Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            )
        } else if job.status == UiJobStatus::Running {
            Some(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
        } else if job.status == UiJobStatus::Pending {
            Some(
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            None
        };

        let mut spans = Vec::new();
        spans.push(Span::styled(
            icon_char.to_string(),
            Self::apply_highlight(
                Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
                highlight,
            ),
        ));
        spans.push(Span::styled(
            format!(" {}", job.name),
            Self::apply_highlight(Style::default(), highlight),
        ));
        spans.push(Span::styled(
            format!(" [{}]", job.stage),
            Self::apply_highlight(Style::default().fg(Color::DarkGray), highlight),
        ));
        spans.push(Span::styled(
            format!(" ({})", job.log_hash),
            Self::apply_highlight(Style::default().fg(Color::Yellow), highlight),
        ));
        match job.status {
            UiJobStatus::Running => {
                spans.push(Span::styled(
                    " • RUNNING",
                    Self::apply_highlight(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                        highlight,
                    ),
                ));
            }
            UiJobStatus::Pending => {
                spans.push(Span::styled(
                    " • WAITING ON DEPS",
                    Self::apply_highlight(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                        highlight,
                    ),
                ));
            }
            _ => {}
        }

        spans
    }

    pub(super) fn apply_highlight(base: Style, highlight: Option<Style>) -> Style {
        if let Some(highlight_style) = highlight {
            base.patch(highlight_style)
        } else {
            base
        }
    }

    pub(super) fn help_prompt(&self) -> Paragraph<'static> {
        Paragraph::new(vec![Line::from(vec![
            Self::hint_label("Help", Color::Cyan),
            Span::raw(": press "),
            key_span_color("?", Color::Yellow),
            Span::raw(" for shortcuts"),
        ])])
        .wrap(Wrap { trim: false })
    }

    pub(super) fn plan_text(&self) -> String {
        if self.plan_text.is_empty() {
            "plan unavailable (run opal plan?)".to_string()
        } else {
            self.plan_text.clone()
        }
    }

    fn resource_line(connector: &str, label: &str, color: Color) -> Line<'static> {
        Line::from(vec![
            Span::styled(
                format!("     {} ", connector),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(label.to_string(), Style::default().fg(color)),
        ])
    }

    fn summarize_list(items: &[String]) -> String {
        const MAX: usize = 3;
        if items.len() <= MAX {
            items.join(", ")
        } else {
            let shown = items
                .iter()
                .take(MAX)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            format!("{shown} (+{} more)", items.len() - MAX)
        }
    }

    fn relative_display(&self, path: &str) -> String {
        let raw = Path::new(path);
        if raw.is_absolute()
            && let Ok(relative) = raw.strip_prefix(&self.workdir)
        {
            let display = relative.display().to_string();
            if display.is_empty() {
                ".".to_string()
            } else {
                format!("./{display}")
            }
        } else {
            path.to_string()
        }
    }

    fn current_help_lines(&self) -> Vec<Line<'static>> {
        match self.help_view {
            HelpView::Shortcuts => self.shortcut_help_lines(),
            HelpView::Document(idx) => self.help_document_lines(idx),
        }
    }

    fn max_help_scroll(&self) -> u16 {
        let lines = self.current_help_lines();
        let viewport = self.help_viewport as usize;
        if viewport == 0 {
            return 0;
        }
        let width = usize::from(self.help_text_width());
        let total_rows = total_rows(&lines, width);
        if total_rows <= viewport {
            0
        } else {
            (total_rows - viewport).min(u16::MAX as usize) as u16
        }
    }

    pub(super) fn help_visible(&self) -> bool {
        self.show_help
    }

    pub(super) fn update_help_viewport(&mut self, width: u16, height: u16) {
        self.help_viewport = height.saturating_sub(2).max(1);
        self.help_width = width.saturating_sub(2).max(1);
        let max_scroll = self.max_help_scroll();
        if self.help_scroll > max_scroll {
            self.help_scroll = max_scroll;
        }
    }

    pub(super) fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
        if self.show_help {
            self.help_view = HelpView::Shortcuts;
            self.help_scroll = 0;
        }
    }

    pub(super) fn help_window_title(&self) -> String {
        match self.help_view {
            HelpView::Shortcuts => "Help".to_string(),
            HelpView::Document(idx) => {
                if let Some(doc) = self.help_docs.get(idx) {
                    format!("Help — {}", doc.title)
                } else {
                    "Help".to_string()
                }
            }
        }
    }

    pub(super) fn help_header(&self) -> Paragraph<'static> {
        match self.help_view {
            HelpView::Shortcuts => Paragraph::new(vec![
                Line::from(Span::styled(
                    "OPAL HELP",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Keyboard shortcuts and local docs",
                    Style::default().fg(Color::DarkGray),
                )),
            ])
            .alignment(Alignment::Center),
            HelpView::Document(idx) => {
                if let Some(doc) = self.help_docs.get(idx) {
                    let total = self.help_docs.len();
                    Paragraph::new(vec![
                        Line::from(Span::styled(
                            doc.title.clone(),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::from(vec![
                            Span::styled(
                                format!("Document {}/{}", idx + 1, total),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::raw("  "),
                            Span::styled(
                                doc.path.display().to_string(),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]),
                    ])
                    .alignment(Alignment::Center)
                } else {
                    Paragraph::new(vec![Line::from("Document unavailable")])
                        .alignment(Alignment::Center)
                }
            }
        }
    }

    pub(super) fn help_body(&self) -> Paragraph<'static> {
        let lines = self.current_help_lines();
        let mut body = Paragraph::new(lines).wrap(Wrap { trim: false });
        if self.max_help_scroll() > 0 {
            let scroll = self.help_scroll.min(self.max_help_scroll());
            body = body.scroll((scroll, 0));
        }
        body
    }

    pub(super) fn help_footer(&self) -> Paragraph<'static> {
        let mut spans = vec![
            Span::raw("Press "),
            key_span_color("?", Color::Yellow),
            Span::raw(" or "),
            key_span_color("Esc", Color::Yellow),
            Span::raw(" to close"),
        ];
        if !self.help_docs.is_empty() {
            spans.push(Span::raw("  "));
            spans.push(bullet());
            spans.push(Span::raw("Press "));
            spans.push(key_span_color("1-9", Color::Cyan));
            spans.push(Span::raw(" to open docs"));
        }
        if matches!(self.help_view, HelpView::Document(_)) && !self.help_docs.is_empty() {
            spans.push(Span::raw("  "));
            spans.push(bullet());
            spans.push(Span::raw("Use "));
            spans.push(key_span_color("←/→", Color::Cyan));
            spans.push(Span::raw(" to switch docs"));
            spans.push(Span::raw("  "));
            spans.push(bullet());
            spans.push(Span::raw("Use "));
            spans.push(key_span_color("↑/↓/Pg", Color::Cyan));
            spans.push(Span::raw(" to scroll"));
            spans.push(Span::raw("  "));
            spans.push(bullet());
            spans.push(Span::raw("Press "));
            spans.push(key_span_color("S", Color::Cyan));
            spans.push(Span::raw(" for shortcuts"));
        }
        Paragraph::new(vec![Line::from(spans)])
            .alignment(Alignment::Right)
            .wrap(Wrap { trim: false })
    }

    pub(super) fn show_help_shortcuts(&mut self) {
        self.help_view = HelpView::Shortcuts;
        self.help_scroll = 0;
    }

    pub(super) fn open_help_document(&mut self, index: usize) {
        if index < self.help_docs.len() {
            self.help_view = HelpView::Document(index);
            self.help_scroll = 0;
        }
    }

    pub(super) fn open_help_document_digit(&mut self, digit: char) {
        if let Some(value) = digit.to_digit(10) {
            if value == 0 {
                return;
            }
            let idx = (value - 1) as usize;
            self.open_help_document(idx);
        }
    }

    pub(super) fn next_help_document(&mut self) {
        if self.help_docs.is_empty() {
            return;
        }
        match self.help_view {
            HelpView::Shortcuts => self.open_help_document(0),
            HelpView::Document(idx) => {
                let next = (idx + 1) % self.help_docs.len();
                self.open_help_document(next);
            }
        }
    }

    pub(super) fn previous_help_document(&mut self) {
        if self.help_docs.is_empty() {
            return;
        }
        match self.help_view {
            HelpView::Shortcuts => self.open_help_document(self.help_docs.len().saturating_sub(1)),
            HelpView::Document(idx) => {
                let prev = if idx == 0 {
                    self.help_docs.len() - 1
                } else {
                    idx - 1
                };
                self.open_help_document(prev);
            }
        }
    }

    pub(super) fn scroll_help(&mut self, delta: i32) {
        let max_scroll = self.max_help_scroll() as i32;
        if max_scroll <= 0 {
            self.help_scroll = 0;
            return;
        }
        let current = self.help_scroll as i32;
        let next = (current + delta).clamp(0, max_scroll);
        self.help_scroll = next as u16;
    }

    pub(super) fn scroll_help_to_top(&mut self) {
        self.help_scroll = 0;
    }

    pub(super) fn scroll_help_to_bottom(&mut self) {
        self.help_scroll = self.max_help_scroll();
    }

    pub(super) fn scroll_help_page_up(&mut self) {
        let delta = self.help_viewport as i32;
        self.scroll_help(-delta.max(1));
    }

    pub(super) fn scroll_help_page_down(&mut self) {
        let delta = self.help_viewport as i32;
        self.scroll_help(delta.max(1));
    }

    fn shortcut_help_lines(&self) -> Vec<Line<'static>> {
        let sections = [
            Self::help_section(
                "Jobs",
                Color::Green,
                &[
                    ("j/k/←/→", "change tab"),
                    ("↓/↑", "next/prev"),
                    ("r", "restart job"),
                    ("o", "open log"),
                    ("x", "cancel job"),
                ],
            ),
            Self::help_section("Manual", Color::Yellow, &[("m", "start pending job")]),
            Self::help_section(
                "Logs",
                Color::Magenta,
                &[
                    ("Shift/Ctrl+↑/↓", "scroll"),
                    ("g/G", "top/bottom"),
                    ("Ctrl+u/d", "half page"),
                    ("Ctrl+f/b", "page"),
                    ("Ctrl+e/y", "line"),
                    ("Space", "page down"),
                ],
            ),
            Self::help_section(
                "History/Panes",
                Color::Cyan,
                &[
                    ("↑/↓/j/k", "move cursor"),
                    ("←/→/h/l", "collapse"),
                    ("Enter/Space", "open run/log"),
                    ("Tab", "switch panes"),
                    ("q", "quit"),
                ],
            ),
            Self::help_section("Plan", Color::White, &[("p", "open plan in pager")]),
        ];

        let mut lines = Vec::new();
        lines.push(Line::from(Span::raw("")));
        for chunk in sections.chunks(2) {
            let left = chunk.first().cloned().unwrap_or_default();
            let right = chunk.get(1).cloned().unwrap_or_default();
            lines.extend(Self::merge_help_sections(left, right));
            lines.push(Line::from(Span::raw("")));
        }
        if !self.help_docs.is_empty() {
            lines.extend(self.help_docs_summary_lines());
        }
        lines
    }

    fn help_section(title: &str, color: Color, entries: &[(&str, &str)]) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from(Span::raw(" ")),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    title.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::raw(" ")),
        ];
        for (key, desc) in entries {
            lines.push(Line::from(vec![
                Span::raw("    "),
                key_span_color(key, color),
            ]));
            lines.push(Line::from(vec![
                Span::raw("       "),
                Span::raw((*desc).to_string()),
            ]));
            lines.push(Line::from(Span::raw(" ")));
        }
        lines
    }

    fn merge_help_sections(
        left: Vec<Line<'static>>,
        right: Vec<Line<'static>>,
    ) -> Vec<Line<'static>> {
        if right.is_empty() {
            return left;
        }
        let width = 36usize;
        let mut merged = Vec::new();
        let max = left.len().max(right.len());
        for idx in 0..max {
            let left_line = left.get(idx).cloned().unwrap_or_else(|| Line::from(""));
            let right_line = right.get(idx).cloned().unwrap_or_else(|| Line::from(""));
            let mut spans = left_line.spans.clone();
            let pad = width.saturating_sub(spans.iter().map(|s| s.width()).sum::<usize>());
            spans.push(Span::raw(" ".repeat(pad + 4)));
            spans.extend(right_line.spans.iter().cloned());
            merged.push(Line::from(spans));
        }
        merged
    }

    fn help_docs_summary_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![
            Line::from(Span::raw(" ")),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "Reference",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw("    "),
                Span::raw("Press a number to open a document"),
            ]),
            Line::from(vec![
                Span::raw("    "),
                Span::raw("Use S to return here after reading"),
            ]),
            Line::from(Span::raw(" ")),
        ];
        let quick_docs = self.help_docs.iter().take(9);
        for (idx, doc) in quick_docs.enumerate() {
            let label = format!("{}", idx + 1);
            lines.push(Line::from(vec![
                Span::raw("     "),
                key_span_color(&label, Color::Cyan),
                Span::raw("  "),
                Span::styled(doc.title.clone(), Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(vec![
                Span::raw("        "),
                Span::styled(
                    doc.path.display().to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            lines.push(Line::from(Span::raw(" ")));
        }
        if self.help_docs.len() > 9 {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    format!(
                        "Use ←/→ to browse the remaining {} file(s)",
                        self.help_docs.len() - 9
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        lines
    }

    fn help_document_lines(&self, index: usize) -> Vec<Line<'static>> {
        if let Some(doc) = self.help_docs.get(index) {
            if doc.lines.is_empty() {
                vec![Line::from("Document is empty")]
            } else {
                doc.lines.clone()
            }
        } else {
            vec![Line::from("Document not found")]
        }
    }

    fn hint_label(text: &str, color: Color) -> Span<'static> {
        Span::styled(
            format!("{text}: "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )
    }

    pub(super) fn info_panel(&self) -> Paragraph<'_> {
        let job = match self.active_job() {
            Some(job) => job,
            None => {
                return Paragraph::new(vec![Line::from("No job selected")])
                    .block(Block::default().borders(Borders::ALL).title("Details"))
                    .wrap(Wrap { trim: true });
            }
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Stage: ", Style::default().fg(Color::Cyan)),
                Span::raw(job.stage.clone()),
            ]),
            Line::from(vec![
                Span::styled("Log: ", Style::default().fg(Color::Cyan)),
                Span::raw(job.log_path.display().to_string()),
            ]),
            Line::from(vec![
                Span::styled("Status: ", Style::default().fg(Color::Cyan)),
                Span::raw(format!(
                    "{} ({:.2}s)",
                    job.status.label(),
                    job.display_duration()
                )),
            ]),
        ];
        if job.manual_pending {
            lines.push(Line::from(vec![
                Span::styled("Manual: ", Style::default().fg(Color::Yellow)),
                Span::raw("waiting (press 'm' to start)"),
            ]));
        }

        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Details"))
            .wrap(Wrap { trim: true })
    }

    pub(super) fn log_view(
        &self,
        pipeline_finished: bool,
        width: u16,
        height: u16,
    ) -> Paragraph<'_> {
        if self.history_preview.is_some() {
            return self.history_preview_view(width, height);
        }
        let job = match self.active_job() {
            Some(job) => job,
            None => {
                return Paragraph::new(vec![Line::from("No job selected")])
                    .block(Block::default().borders(Borders::ALL).title("Logs"))
                    .wrap(Wrap { trim: true });
            }
        };
        let mut lines: Vec<Line> = Vec::new();
        let status_span = Span::styled(
            format!(
                "Status: {} ({:.2}s)",
                job.status.label(),
                job.display_duration()
            ),
            Style::default().fg(job.status.icon().1),
        );
        lines.push(Line::from(status_span));
        if let Some(err) = &job.error {
            lines.push(Line::from(Span::styled(
                format!("Error: {}", err),
                Style::default().fg(Color::Red),
            )));
        }
        lines.push(Line::from(Span::raw(" ")));

        if pipeline_finished {
            lines.push(Line::from(Span::styled(
                "Pipeline complete – press q to exit",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::raw(" ")));
        }

        let inner_height = height.saturating_sub(2);
        let inner_width = width.saturating_sub(2).max(1) as usize;
        let header_rows = total_rows(&lines, inner_width);
        let available_rows = inner_height.saturating_sub(header_rows as u16) as usize;
        lines.extend(job.visible_logs(inner_width, available_rows));

        let mut title = "Logs".to_string();
        if job.status.is_done() {
            title.push_str(" (complete)");
        }

        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false })
    }

    pub(super) fn current_log_path(&self) -> Option<PathBuf> {
        self.active_job().map(|job| job.log_path.clone())
    }

    pub(super) fn restartable_job_name(&self) -> Option<String> {
        if self.history_view.is_some() {
            return None;
        }
        self.jobs
            .get(self.selected)
            .and_then(|job| job.status.is_restartable().then(|| job.name.clone()))
    }

    pub(super) fn cancelable_job_name(&self) -> Option<String> {
        if self.history_view.is_some() {
            return None;
        }
        self.jobs
            .get(self.selected)
            .and_then(|job| (job.status == UiJobStatus::Running).then(|| job.name.clone()))
    }

    pub(super) fn manual_job_name(&self) -> Option<String> {
        if self.history_view.is_some() {
            return None;
        }
        self.jobs
            .get(self.selected)
            .and_then(|job| job.manual_pending.then(|| job.name.clone()))
    }

    pub(super) fn restart_job(&mut self, name: &str) {
        if let Some(idx) = self.order.get(name).copied() {
            self.jobs[idx].reset_for_restart();
        }
    }

    pub(super) fn set_manual_pending(&mut self, name: &str, pending: bool) {
        if let Some(idx) = self.order.get(name).copied() {
            self.jobs[idx].manual_pending = pending;
            if pending {
                self.jobs[idx].status = UiJobStatus::Pending;
            }
        }
    }

    pub(super) fn job_started(&mut self, name: &str) {
        if let Some(idx) = self.order.get(name).copied() {
            self.jobs[idx].manual_pending = false;
            self.jobs[idx].status = UiJobStatus::Running;
            self.jobs[idx].duration = 0.0;
            self.jobs[idx].start_time = Some(Instant::now());
        }
    }

    pub(super) fn push_log(&mut self, name: &str, line: String) {
        if let Some(idx) = self.order.get(name).copied() {
            self.jobs[idx].push_log(line);
        }
    }

    pub(super) fn finish_job(
        &mut self,
        name: &str,
        status: UiJobStatus,
        duration: f32,
        error: Option<String>,
    ) {
        if let Some(idx) = self.order.get(name).copied() {
            self.jobs[idx].status = status;
            self.jobs[idx].duration = duration;
            self.jobs[idx].start_time = None;
            self.jobs[idx].error = error;
            self.jobs[idx].manual_pending = false;
        }
    }

    pub(super) fn next_job(&mut self) {
        let len = self.active_jobs().len();
        if len == 0 {
            return;
        }
        self.clear_history_preview();
        let next = (self.active_selected_index() + 1) % len;
        self.set_active_selected_index(next);
    }

    pub(super) fn previous_job(&mut self) {
        let len = self.active_jobs().len();
        if len == 0 {
            return;
        }
        self.clear_history_preview();
        let current = self.active_selected_index();
        let prev = if current == 0 { len - 1 } else { current - 1 };
        self.set_active_selected_index(prev);
    }

    pub(super) fn select_job(&mut self, idx: usize) {
        let len = self.active_jobs().len();
        if idx >= len {
            return;
        }
        self.clear_history_preview();
        self.focus = PaneFocus::Jobs;
        self.set_active_selected_index(idx);
    }

    pub(super) fn scroll_logs_line_up(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_up(LOG_SCROLL_STEP);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_up(LOG_SCROLL_STEP);
        }
    }

    pub(super) fn scroll_logs_line_down(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_down(LOG_SCROLL_STEP);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_down(LOG_SCROLL_STEP);
        }
    }

    pub(super) fn scroll_logs_half_up(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_up(LOG_SCROLL_HALF);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_up(LOG_SCROLL_HALF);
        }
    }

    pub(super) fn scroll_logs_half_down(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_down(LOG_SCROLL_HALF);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_down(LOG_SCROLL_HALF);
        }
    }

    pub(super) fn scroll_logs_page_up(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_up(LOG_SCROLL_PAGE);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_up(LOG_SCROLL_PAGE);
        }
    }

    pub(super) fn scroll_logs_page_down(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_down(LOG_SCROLL_PAGE);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_down(LOG_SCROLL_PAGE);
        }
    }

    pub(super) fn scroll_logs_mouse_up(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_up(LOG_SCROLL_STEP);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_up(LOG_SCROLL_STEP);
        }
    }

    pub(super) fn scroll_logs_mouse_down(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_down(LOG_SCROLL_STEP);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_down(LOG_SCROLL_STEP);
        }
    }

    pub(super) fn scroll_top(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_to_top();
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_to_top();
        }
    }

    pub(super) fn scroll_bottom(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_to_bottom();
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_to_bottom();
        }
    }
}

fn key_span_color(text: &str, color: Color) -> Span<'static> {
    Span::styled(
        text.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn bullet() -> Span<'static> {
    Span::styled(" • ", Style::default().fg(Color::DarkGray))
}

pub(super) struct UiJobState {
    name: String,
    stage: String,
    log_path: PathBuf,
    log_hash: String,
    status: UiJobStatus,
    duration: f32,
    start_time: Option<Instant>,
    error: Option<String>,
    logs: Vec<String>,
    scroll_offset: usize,
    follow_logs: bool,
    manual_pending: bool,
}

impl UiJobState {
    fn from(info: UiJobInfo) -> Self {
        Self {
            name: info.name,
            stage: info.stage,
            log_path: info.log_path,
            log_hash: info.log_hash,
            status: UiJobStatus::Pending,
            duration: 0.0,
            start_time: None,
            error: None,
            logs: Vec::new(),
            scroll_offset: 0,
            follow_logs: true,
            manual_pending: false,
        }
    }

    fn from_history(run_id: &str, job: &HistoryJob) -> Self {
        let log_path =
            job.log_path.as_ref().map(PathBuf::from).unwrap_or_else(|| {
                PathBuf::from(format!(".opal/logs/{run_id}/{}.log", job.log_hash))
            });
        Self {
            name: job.name.clone(),
            stage: job.stage.clone(),
            log_path,
            log_hash: job.log_hash.clone(),
            status: UiJobStatus::from_history(job.status),
            duration: 0.0,
            start_time: None,
            error: None,
            logs: Vec::new(),
            scroll_offset: 0,
            follow_logs: true,
            manual_pending: false,
        }
    }

    fn push_log(&mut self, line: String) {
        self.logs.push(line);
        if self.follow_logs {
            self.scroll_offset = 0;
        }
    }

    fn auto_follow(&mut self) {
        self.scroll_offset = 0;
        self.follow_logs = true;
    }

    fn reset_for_restart(&mut self) {
        self.status = UiJobStatus::Pending;
        self.duration = 0.0;
        self.start_time = None;
        self.error = None;
        self.logs.clear();
        self.scroll_offset = 0;
        self.follow_logs = true;
    }

    fn scroll_lines_up(&mut self, lines: usize) {
        self.scroll_by(lines, ScrollDirection::Up);
    }

    fn scroll_lines_down(&mut self, lines: usize) {
        self.scroll_by(lines, ScrollDirection::Down);
    }

    fn scroll_by(&mut self, lines: usize, direction: ScrollDirection) {
        if self.logs.is_empty() || lines == 0 {
            return;
        }
        match direction {
            ScrollDirection::Up => {
                let max_scroll = self.logs.len().saturating_sub(1);
                self.scroll_offset = (self.scroll_offset + lines).min(max_scroll);
                self.follow_logs = false;
            }
            ScrollDirection::Down => {
                self.scroll_offset = self.scroll_offset.saturating_sub(lines);
                if self.scroll_offset == 0 {
                    self.follow_logs = true;
                }
            }
        }
    }

    fn scroll_to_top(&mut self) {
        self.scroll_offset = self.logs.len().saturating_sub(1);
        if self.scroll_offset > 0 {
            self.follow_logs = false;
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.follow_logs = true;
    }

    fn visible_logs(&self, wrap_width: usize, max_rows: usize) -> Vec<Line<'static>> {
        if self.logs.is_empty() {
            return vec![Line::from("(no output yet)")];
        }

        let wrap_width = wrap_width.max(1);
        let mut remaining_rows = max_rows.max(1);
        let total = self.logs.len();
        let offset = self.scroll_offset.min(total.saturating_sub(1));
        let mut end = total.saturating_sub(offset);
        if end == 0 {
            end = total;
        }

        let mut collected: Vec<Line<'static>> = Vec::new();
        while end > 0 {
            let idx = end - 1;
            let line = format_log_entry(&self.logs[idx]);
            let line_rows = rows_for_line(&line, wrap_width);
            if line_rows > remaining_rows && !collected.is_empty() {
                break;
            }
            let consumed = line_rows.min(remaining_rows);
            collected.push(line);
            remaining_rows = remaining_rows.saturating_sub(consumed);
            end -= 1;
            if remaining_rows == 0 {
                break;
            }
        }

        collected.reverse();
        collected
    }

    fn display_duration(&self) -> f32 {
        if matches!(self.status, UiJobStatus::Running)
            && let Some(start) = self.start_time
        {
            return start.elapsed().as_secs_f32();
        }
        self.duration
    }
}

enum ScrollDirection {
    Up,
    Down,
}

pub(super) struct HistoryRenderNode {
    key: HistoryNodeKey,
    parent_index: Option<usize>,
    line: Line<'static>,
}

#[derive(Clone)]
enum HistoryNodeKey {
    CurrentRun,
    CurrentJob(usize),
    FinishedRun {
        run_id: String,
    },
    FinishedJob {
        run_id: String,
        job_name: String,
        log_path: Option<PathBuf>,
    },
    ResourceDir {
        title: String,
        path: PathBuf,
    },
    ResourceInfo,
}

struct HistoryRunView {
    run_id: String,
    jobs: Vec<UiJobState>,
    selected: usize,
}

struct HistoryPreview {
    title: String,
    path: PathBuf,
    lines: Vec<String>,
    scroll_offset: usize,
}

struct ResourceEntry {
    key: HistoryNodeKey,
    label: String,
    color: Color,
}

impl ResourceEntry {
    fn dir(label: String, path: PathBuf) -> Self {
        Self {
            key: HistoryNodeKey::ResourceDir {
                title: label.clone(),
                path,
            },
            label,
            color: Color::Cyan,
        }
    }

    fn info(label: String) -> Self {
        Self {
            key: HistoryNodeKey::ResourceInfo,
            label,
            color: Color::DarkGray,
        }
    }
}

impl HistoryPreview {
    fn visible_lines(&self, wrap_width: usize, max_rows: usize) -> Vec<Line<'static>> {
        if self.lines.is_empty() {
            return vec![Line::from("(empty log)")];
        }

        let wrap_width = wrap_width.max(1);
        let mut remaining_rows = max_rows.max(1);
        let total = self.lines.len();
        let offset = self.scroll_offset.min(total.saturating_sub(1));
        let mut end = total.saturating_sub(offset);
        if end == 0 {
            end = total;
        }

        let mut collected = Vec::new();
        while end > 0 {
            let idx = end - 1;
            let line = format_log_entry(&self.lines[idx]);
            let line_rows = rows_for_line(&line, wrap_width);
            if line_rows > remaining_rows && !collected.is_empty() {
                break;
            }
            let consumed = line_rows.min(remaining_rows);
            collected.push(line);
            remaining_rows = remaining_rows.saturating_sub(consumed);
            end -= 1;
            if remaining_rows == 0 {
                break;
            }
        }

        collected.reverse();
        collected
    }

    fn scroll_lines_up(&mut self, lines: usize) {
        let max_scroll = self.lines.len().saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + lines).min(max_scroll);
    }

    fn scroll_lines_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    fn scroll_to_top(&mut self) {
        self.scroll_offset = self.lines.len().saturating_sub(1);
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }
}

impl UiJobStatus {
    fn icon(self) -> (&'static str, Color) {
        match self {
            UiJobStatus::Pending => ("•", Color::Gray),
            UiJobStatus::Running => ("↺", Color::Blue),
            UiJobStatus::Success => ("✓", Color::Green),
            UiJobStatus::Failed => ("✗", Color::Red),
            UiJobStatus::Skipped => ("↷", Color::Yellow),
        }
    }

    fn is_done(self) -> bool {
        matches!(
            self,
            UiJobStatus::Success | UiJobStatus::Failed | UiJobStatus::Skipped
        )
    }
}

fn format_log_entry(line: &str) -> Line<'static> {
    if let Some(rest) = line.strip_prefix('[')
        && let Some(idx) = rest.find("] ")
    {
        let meta = &rest[..idx];
        let remainder = &rest[idx + 2..];
        if let Some(space_idx) = meta.rfind(' ') {
            let (timestamp, number) = meta.split_at(space_idx);
            let number = number.trim();
            let mut spans = vec![
                Span::raw("[".to_string()),
                Span::styled(timestamp.to_string(), Style::default().fg(Color::Blue)),
                Span::raw(" ".to_string()),
                Span::styled(number.to_string(), Style::default().fg(Color::Yellow)),
                Span::raw("] ".to_string()),
                Span::raw(remainder.to_string()),
            ];
            if let Some(style) = diff_style(remainder) {
                apply_line_style(&mut spans, style);
            }
            return Line::from(spans);
        }
    }

    let mut spans = vec![Span::raw(line.to_string())];
    if let Some(style) = diff_style(line) {
        apply_line_style(&mut spans, style);
    }
    Line::from(spans)
}

impl HelpDocument {
    fn discover() -> Vec<Self> {
        let mut docs = Vec::new();
        for file in EMBEDDED_DOCS.files() {
            if file
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("md"))
                != Some(true)
            {
                continue;
            }
            if let Some(contents) = file.contents_utf8()
                && let Some(doc) = Self::from_contents(file.path(), contents)
            {
                docs.push(doc);
            }
        }
        docs.sort_by_key(|doc| doc.title.to_lowercase());
        docs
    }

    fn from_contents(path: &Path, contents: &str) -> Option<Self> {
        let path_buf = path.to_path_buf();
        let title = Self::extract_title(contents).unwrap_or_else(|| {
            path_buf
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Document")
                .replace('_', " ")
        });
        let lines = Self::markdown_lines(contents);
        Some(Self {
            title,
            path: path_buf,
            lines,
        })
    }

    fn extract_title(contents: &str) -> Option<String> {
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                let title = trimmed.trim_start_matches('#').trim();
                if !title.is_empty() {
                    return Some(title.to_string());
                }
            }
        }
        None
    }

    fn markdown_lines(contents: &str) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let mut in_code = false;
        for raw in contents.lines() {
            let trimmed_end = raw.trim_end();
            let trimmed = trimmed_end.trim();
            if trimmed.starts_with("```") {
                in_code = !in_code;
                lines.push(Line::from(""));
                continue;
            }
            if trimmed.is_empty() {
                lines.push(Line::from(""));
                continue;
            }
            if in_code {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(trimmed_end.to_string(), Style::default().fg(Color::Green)),
                ]));
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("# ") {
                lines.push(Line::from(Span::styled(
                    rest.trim().to_uppercase(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("## ") {
                lines.push(Line::from(Span::styled(
                    rest.trim().to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("### ") {
                lines.push(Line::from(Span::styled(
                    rest.trim().to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
                continue;
            }
            if let Some(rest) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    bullet(),
                    Span::raw(rest.trim().to_string()),
                ]));
                continue;
            }
            lines.push(Line::from(trimmed_end.to_string()));
        }
        if lines.is_empty() {
            lines.push(Line::from("This document is empty."));
        }
        lines
    }
}

pub(super) fn page_log_with_colors(path: &Path) -> Result<()> {
    let file =
        File::open(path).with_context(|| format!("failed to open log {}", path.display()))?;
    let reader = BufReader::new(file);
    let pager = env::var("PAGER").unwrap_or_else(|_| "less -R".to_string());
    let (cmd, args) = parse_pager_command(&pager);
    let mut child = Command::new(&cmd);
    child.args(&args).stdin(Stdio::piped());

    if let Ok(mut handle) = child.spawn() {
        if let Some(mut stdin) = handle.stdin.take() {
            for line in reader.lines() {
                let line = line?;
                let colored = colorize_log_line(&line);
                writeln!(stdin, "{colored}")?;
            }
        }
        let status = handle.wait()?;
        if status.success() {
            return Ok(());
        }
    }

    let _ = Command::new("cat").arg(path).status();
    Ok(())
}

pub(super) fn page_text_with_pager(content: &str) -> Result<()> {
    let pager = env::var("PAGER").unwrap_or_else(|_| "less -R".to_string());
    let (cmd, args) = parse_pager_command(&pager);
    let mut child = Command::new(&cmd);
    child.args(&args).stdin(Stdio::piped());
    if let Ok(mut handle) = child.spawn() {
        if let Some(mut stdin) = handle.stdin.take() {
            stdin.write_all(content.as_bytes())?;
        }
        let _ = handle.wait();
        return Ok(());
    }
    print!("{content}");
    Ok(())
}

fn parse_pager_command(pager: &str) -> (String, Vec<String>) {
    let mut parts = pager.split_whitespace();
    let cmd = parts
        .next()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "less".to_string());
    let args = parts.map(|s| s.to_string()).collect();
    (cmd, args)
}

fn colorize_log_line(line: &str) -> String {
    if let Some(rest) = line.strip_prefix('[')
        && let Some(idx) = rest.find("] ")
    {
        let meta = &rest[..idx];
        let remainder = &rest[idx + 2..];
        if let Some(space_idx) = meta.rfind(' ') {
            let (timestamp, number) = meta.split_at(space_idx);
            let number = number.trim();
            let body = colorize_diff_body(remainder);
            return format!(
                "[{} {}] {}",
                timestamp.blue().bold(),
                number.yellow().bold(),
                body
            );
        }
    }
    colorize_diff_body(line)
}

fn colorize_diff_body(body: &str) -> String {
    if let Some(rest) = body.strip_prefix('+') {
        format!("{}", format!("+{rest}").green())
    } else if let Some(rest) = body.strip_prefix('-') {
        format!("{}", format!("-{rest}").red())
    } else {
        body.to_string()
    }
}

fn diff_style(text: &str) -> Option<Style> {
    let trimmed = text.trim_start();
    if trimmed.starts_with('+') {
        Some(Style::default().fg(Color::Green))
    } else if trimmed.starts_with('-') {
        Some(Style::default().fg(Color::Red))
    } else {
        None
    }
}

fn apply_line_style(spans: &mut [Span<'static>], style: Style) {
    for span in spans {
        span.style = span.style.patch(style);
    }
}

fn total_rows(lines: &[Line<'_>], width: usize) -> usize {
    lines.iter().map(|line| rows_for_line(line, width)).sum()
}

fn rows_for_line(line: &Line<'_>, width: usize) -> usize {
    let width = width.max(1);
    let text_width = line.width();
    if text_width == 0 {
        1
    } else {
        text_width.div_ceil(width)
    }
}
