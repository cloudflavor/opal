use super::types::{
    CURRENT_HISTORY_KEY, HistoryAction, LOG_SCROLL_HALF, LOG_SCROLL_PAGE, LOG_SCROLL_STEP,
    PaneFocus, UiJobInfo, UiJobResources, UiJobStatus, UiRunnerInfo,
};
use crate::history::{HistoryEntry, HistoryJob, HistoryStatus};
use crate::runtime;
use anyhow::{Context, Result, anyhow};
use include_dir::{Dir, include_dir};
use owo_colors::OwoColorize;
use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, Paragraph, ScrollbarState, Wrap,
};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;
use termimad::MadSkin;
use termimad::minimad::{
    Composite, CompositeStyle, Compound, Line as MarkdownLine, Options, parse_text,
};
use walkdir::WalkDir;

static EMBEDDED_DOCS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/docs");

const HISTORY_TREE_MAX_FS_DEPTH: usize = 3;
const HISTORY_TREE_MAX_FS_ENTRIES: usize = 200;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum TabDensity {
    Auto,
    Compact,
    Full,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LogFilter {
    All,
    Errors,
    Warnings,
    Downloads,
    Build,
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
    collapsed_nodes: HashMap<String, bool>,
    history_preview: Option<HistoryPreview>,
    history_view: Option<HistoryRunView>,
    show_help: bool,
    help_view: HelpView,
    help_scroll: u16,
    help_viewport: u16,
    help_width: u16,
    help_docs: Vec<HelpDocument>,
    job_resources: HashMap<String, UiJobResources>,
    show_history_pane: bool,
    show_job_yaml_pane: bool,
    job_yaml_scroll: u16,
    job_yaml_map: HashMap<String, String>,
    job_yaml_error: Option<String>,
    plan_text: String,
    workdir: PathBuf,
    pipeline_path: PathBuf,
    history_height: u16,
    loaded_dirs: HashSet<PathBuf>,
    has_current_run: bool,
    tab_density: TabDensity,
    log_filter: LogFilter,
}

#[derive(Clone)]
pub(super) struct AiAnalysisSnapshot {
    pub run_id: String,
    pub job_name: String,
    pub source_name: String,
    pub stage: String,
    pub job_yaml: String,
    pub runner_summary: String,
    pub pipeline_summary: String,
    pub runtime_summary: Option<String>,
    pub log_excerpt: String,
    pub failure_hint: Option<String>,
}

impl UiState {
    pub(super) fn new(
        jobs: Vec<UiJobInfo>,
        history: Vec<HistoryEntry>,
        current_run_id: String,
        job_resources: HashMap<String, UiJobResources>,
        plan_text: String,
        workdir: PathBuf,
        pipeline_path: PathBuf,
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

        let mut collapsed_nodes = HashMap::new();
        collapsed_nodes.insert(Self::run_collapse_key(CURRENT_HISTORY_KEY), false);
        for entry in &history {
            collapsed_nodes.insert(Self::run_collapse_key(&entry.run_id), true);
        }

        let has_current_run = !job_states.is_empty();
        let (job_yaml_map, job_yaml_error) = load_job_yaml_map(&pipeline_path)
            .map(|map| (map, None))
            .unwrap_or_else(|err| (HashMap::new(), Some(err.to_string())));

        Self {
            jobs: job_states,
            order,
            selected: 0,
            history,
            current_run_id,
            focus: PaneFocus::Jobs,
            history_selection: 0,
            history_scroll: 0,
            collapsed_nodes,
            history_preview: None,
            history_view: None,
            show_help: false,
            help_view: HelpView::Shortcuts,
            help_scroll: 0,
            help_viewport: 1,
            help_width: 1,
            help_docs: HelpDocument::discover(),
            job_resources,
            show_history_pane: false,
            show_job_yaml_pane: false,
            job_yaml_scroll: 0,
            job_yaml_map,
            job_yaml_error,
            plan_text,
            workdir,
            pipeline_path,
            history_height: 0,
            loaded_dirs: HashSet::new(),
            has_current_run,
            tab_density: TabDensity::Auto,
            log_filter: LogFilter::All,
        }
    }

    fn run_collapse_key(run_id: &str) -> String {
        format!("run:{run_id}")
    }

    fn node_collapse_key(key: &HistoryNodeKey) -> Option<String> {
        match key {
            HistoryNodeKey::CurrentRun => Some(Self::run_collapse_key(CURRENT_HISTORY_KEY)),
            HistoryNodeKey::FinishedRun { run_id } => Some(Self::run_collapse_key(run_id)),
            HistoryNodeKey::ResourceDir { path, .. } => Some(format!("dir:{}", path.display())),
            HistoryNodeKey::FileEntry { path, .. } => Some(format!("fs:{}", path.display())),
            _ => None,
        }
    }

    fn default_collapse_for_key(key: &HistoryNodeKey) -> bool {
        match key {
            HistoryNodeKey::CurrentRun => false,
            HistoryNodeKey::FinishedRun { .. } => true,
            HistoryNodeKey::ResourceDir { .. } => true,
            HistoryNodeKey::FileEntry { is_dir, .. } => *is_dir,
            _ => false,
        }
    }

    fn is_node_collapsed_key(&self, key: &HistoryNodeKey) -> bool {
        if let Some(k) = Self::node_collapse_key(key) {
            self.collapsed_nodes
                .get(&k)
                .copied()
                .unwrap_or(Self::default_collapse_for_key(key))
        } else {
            Self::default_collapse_for_key(key)
        }
    }

    fn set_node_collapsed_key(&mut self, key: &HistoryNodeKey, collapsed: bool) {
        if let Some(k) = Self::node_collapse_key(key) {
            self.collapsed_nodes.insert(k, collapsed);
        }
    }

    fn ensure_dir_loaded(&mut self, path: &Path) {
        if path.exists() {
            self.loaded_dirs.insert(path.to_path_buf());
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

    pub(super) fn history_view_active(&self) -> bool {
        self.history_view.is_some()
    }

    pub(super) fn workdir(&self) -> &Path {
        &self.workdir
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
        let summary = self.pipeline_counts();
        let paragraph = Paragraph::new(lines)
            .block(self.pane_block(
                format!(
                    "Jobs  {}/{} done  run:{} fail:{}",
                    summary.done, summary.total, summary.running, summary.failed
                ),
                !self.focus_is_history(),
            ))
            .wrap(Wrap { trim: false });

        let content_height = rows.saturating_add(2); // account for top/bottom borders

        (paragraph, content_height)
    }

    pub(super) fn history_widget(&mut self, height: u16) -> (List<'static>, ScrollbarState) {
        let nodes = self.history_nodes();
        if nodes.is_empty() {
            self.history_selection = 0;
            self.history_scroll = 0;
            self.clear_history_preview();
            let list = List::new(vec![ListItem::new(Line::from("no runs recorded"))])
                .block(self.pane_block("History", self.focus_is_history()));
            return (list, ScrollbarState::default());
        }

        self.history_height = height;
        self.clamp_and_scroll_history(nodes.len());

        let viewport = Self::history_viewport(height);
        let end = if viewport == 0 {
            nodes.len()
        } else {
            (self.history_scroll + viewport).min(nodes.len())
        };
        let visible = &nodes[self.history_scroll..end];

        let items: Vec<ListItem<'static>> = visible
            .iter()
            .enumerate()
            .map(|(offset, node)| {
                let idx = self.history_scroll + offset;
                let line = node.line.clone();
                if self.focus == PaneFocus::History && idx == self.history_selection {
                    ListItem::new(Self::apply_history_highlight(line))
                } else {
                    ListItem::new(line)
                }
            })
            .collect();

        let list = List::new(items).block(self.pane_block("History", self.focus_is_history()));
        let scrollbar = ScrollbarState::new(nodes.len()).position(self.history_scroll);
        (list, scrollbar)
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
                .block(self.pane_block("Logs", self.focus_is_jobs()))
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
            .block(self.pane_block("Logs", self.focus_is_jobs()))
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
        let tree = self.history_tree_entries();
        self.flatten_history_tree(&tree)
    }

    fn history_tree_entries(&self) -> Vec<HistoryTreeEntry> {
        let mut roots = Vec::new();
        if self.has_current_run {
            roots.push(self.current_run_tree_entry());
        }
        for entry in self.history.iter().rev() {
            roots.push(self.finished_run_tree_entry(entry));
        }
        roots
    }

    fn current_run_tree_entry(&self) -> HistoryTreeEntry {
        let mut children = Vec::new();
        for (idx, job) in self.jobs.iter().enumerate() {
            let resources = self.job_resources.get(&job.name);
            children.push(HistoryTreeEntry {
                key: HistoryNodeKey::CurrentJob(idx),
                display: HistoryNodeDisplay::Job(JobDisplay {
                    name: job.name.clone(),
                    stage: job.stage.clone(),
                    hash: job.log_hash.clone(),
                    status: Self::history_status_from_ui(job.status),
                }),
                children: resources
                    .map(|res| self.resource_tree_entries(res))
                    .unwrap_or_default(),
                collapsed: false,
            });
        }
        HistoryTreeEntry {
            key: HistoryNodeKey::CurrentRun,
            display: HistoryNodeDisplay::RunHeader(RunHeaderDisplay {
                run_id: self.current_run_id.clone(),
                status: self.current_run_status(),
                kind: RunHeaderKind::Current,
                viewing: self.history_view.is_none(),
            }),
            children,
            collapsed: self.is_run_collapsed(CURRENT_HISTORY_KEY),
        }
    }

    fn finished_run_tree_entry(&self, entry: &HistoryEntry) -> HistoryTreeEntry {
        let mut children = Vec::new();
        for job in &entry.jobs {
            let resources = UiJobResources::from(job);
            children.push(HistoryTreeEntry {
                key: HistoryNodeKey::FinishedJob {
                    run_id: entry.run_id.clone(),
                    job_name: job.name.clone(),
                },
                display: HistoryNodeDisplay::Job(JobDisplay {
                    name: job.name.clone(),
                    stage: job.stage.clone(),
                    hash: job.log_hash.clone(),
                    status: job.status,
                }),
                children: self.resource_tree_entries(&resources),
                collapsed: false,
            });
        }
        HistoryTreeEntry {
            key: HistoryNodeKey::FinishedRun {
                run_id: entry.run_id.clone(),
            },
            display: HistoryNodeDisplay::RunHeader(RunHeaderDisplay {
                run_id: entry.run_id.clone(),
                status: entry.status,
                kind: RunHeaderKind::Finished {
                    finished_at: entry.finished_at.clone(),
                },
                viewing: self
                    .history_view
                    .as_ref()
                    .map(|view| view.run_id == entry.run_id)
                    .unwrap_or(false),
            }),
            children,
            collapsed: self.is_run_collapsed(&entry.run_id),
        }
    }

    fn resource_tree_entries(&self, resources: &UiJobResources) -> Vec<HistoryTreeEntry> {
        let mut nodes = Vec::new();
        if let Some(container) = &resources.container_name {
            nodes.push(HistoryTreeEntry {
                key: HistoryNodeKey::ResourceInfo,
                display: HistoryNodeDisplay::Resource(ResourceDisplay::Info {
                    label: format!("Container: {container}"),
                    color: Color::Cyan,
                }),
                children: Vec::new(),
                collapsed: false,
            });
        }
        if let Some(network) = &resources.service_network {
            nodes.push(HistoryTreeEntry {
                key: HistoryNodeKey::ResourceInfo,
                display: HistoryNodeDisplay::Resource(ResourceDisplay::Info {
                    label: format!("Service network: {network}"),
                    color: Color::Cyan,
                }),
                children: Vec::new(),
                collapsed: false,
            });
        }
        if !resources.service_containers.is_empty() {
            nodes.push(HistoryTreeEntry {
                key: HistoryNodeKey::ResourceInfo,
                display: HistoryNodeDisplay::Resource(ResourceDisplay::Info {
                    label: format!(
                        "Service containers: {}",
                        Self::summarize_list(&resources.service_containers)
                    ),
                    color: Color::Cyan,
                }),
                children: Vec::new(),
                collapsed: false,
            });
        }
        if let Some(path) = &resources.runtime_summary_path {
            let path = PathBuf::from(path);
            let title = path
                .file_name()
                .map(|name| format!("Runtime summary: {}", name.to_string_lossy()))
                .unwrap_or_else(|| format!("Runtime summary: {}", path.display()));
            nodes.push(HistoryTreeEntry {
                key: HistoryNodeKey::FileEntry {
                    path: path.clone(),
                    is_dir: false,
                },
                display: HistoryNodeDisplay::Resource(ResourceDisplay::Directory { title }),
                children: Vec::new(),
                collapsed: false,
            });
        }
        if let Some(dir) = &resources.artifact_dir {
            let title = format!("Artifact dir: {}", self.relative_display(dir));
            let path = PathBuf::from(dir);
            let key = HistoryNodeKey::ResourceDir {
                title: title.clone(),
                path: path.clone(),
            };
            let children = if self.loaded_dirs.contains(&path) {
                self.build_fs_tree_entries(&path, 0)
            } else {
                Vec::new()
            };
            nodes.push(HistoryTreeEntry {
                key: key.clone(),
                display: HistoryNodeDisplay::Resource(ResourceDisplay::Directory { title }),
                children,
                collapsed: self.is_node_collapsed_key(&key),
            });
        }
        if !resources.artifact_paths.is_empty() {
            let display_paths: Vec<String> = resources
                .artifact_paths
                .iter()
                .map(|p| self.relative_display(p))
                .collect();
            let summary = Self::summarize_list(&display_paths);
            nodes.push(HistoryTreeEntry {
                key: HistoryNodeKey::ResourceInfo,
                display: HistoryNodeDisplay::Resource(ResourceDisplay::Info {
                    label: format!("Artifacts: {summary}"),
                    color: Color::DarkGray,
                }),
                children: Vec::new(),
                collapsed: false,
            });
        }
        for cache in &resources.caches {
            let title = format!("Cache {} ({})", cache.key, cache.policy);
            let path = PathBuf::from(&cache.host);
            let key = HistoryNodeKey::ResourceDir {
                title: title.clone(),
                path: path.clone(),
            };
            let children = if self.loaded_dirs.contains(&path) {
                self.build_fs_tree_entries(&path, 0)
            } else {
                Vec::new()
            };
            nodes.push(HistoryTreeEntry {
                key: key.clone(),
                display: HistoryNodeDisplay::Resource(ResourceDisplay::Directory { title }),
                children,
                collapsed: self.is_node_collapsed_key(&key),
            });
        }
        nodes
    }

    fn build_fs_tree_entries(&self, path: &Path, depth: usize) -> Vec<HistoryTreeEntry> {
        if depth >= HISTORY_TREE_MAX_FS_DEPTH || !path.exists() {
            return Vec::new();
        }
        let read_dir = match fs::read_dir(path) {
            Ok(dir) => dir,
            Err(_) => return Vec::new(),
        };
        let mut entries: Vec<_> = read_dir.filter_map(|entry| entry.ok()).collect();
        entries.sort_by_key(|entry| entry.file_name());
        let mut nodes = Vec::new();
        for entry in entries.into_iter().take(HISTORY_TREE_MAX_FS_ENTRIES) {
            let name = entry.file_name().to_string_lossy().to_string();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let entry_path = entry.path();
            let is_dir = file_type.is_dir();
            let key = HistoryNodeKey::FileEntry {
                path: entry_path.clone(),
                is_dir,
            };
            let children = if is_dir {
                self.build_fs_tree_entries(&entry_path, depth + 1)
            } else {
                Vec::new()
            };
            nodes.push(HistoryTreeEntry {
                key: key.clone(),
                display: HistoryNodeDisplay::FileEntry(FileEntryDisplay { name, is_dir }),
                children,
                collapsed: if is_dir {
                    self.is_node_collapsed_key(&key)
                } else {
                    false
                },
            });
        }
        nodes
    }

    fn flatten_history_tree(&self, roots: &[HistoryTreeEntry]) -> Vec<HistoryRenderNode> {
        let mut nodes = Vec::new();
        for entry in roots {
            self.flatten_history_tree_node(entry, &mut nodes, None, None);
        }
        nodes
    }

    fn flatten_history_tree_node(
        &self,
        entry: &HistoryTreeEntry,
        nodes: &mut Vec<HistoryRenderNode>,
        parent_index: Option<usize>,
        connector: Option<&str>,
    ) {
        let line = self.render_history_tree_line(entry, connector);
        nodes.push(HistoryRenderNode {
            key: entry.key.clone(),
            parent_index,
            display: entry.display.clone(),
            line,
        });
        let current_index = nodes.len() - 1;
        if entry.collapsed {
            return;
        }
        let total = entry.children.len();
        for (idx, child) in entry.children.iter().enumerate() {
            let child_connector = if idx + 1 == total { "└─" } else { "├─" };
            self.flatten_history_tree_node(
                child,
                nodes,
                Some(current_index),
                Some(child_connector),
            );
        }
    }

    fn render_history_tree_line(
        &self,
        entry: &HistoryTreeEntry,
        connector: Option<&str>,
    ) -> Line<'static> {
        match &entry.display {
            HistoryNodeDisplay::RunHeader(header) => Self::run_header_line(header, entry.collapsed),
            HistoryNodeDisplay::Job(job) => {
                let symbol = connector.unwrap_or("└─");
                Self::history_job_line(symbol, &job.name, &job.stage, &job.hash, job.status)
            }
            HistoryNodeDisplay::Resource(ResourceDisplay::Directory { title }) => {
                let symbol = connector.unwrap_or("└─");
                Self::resource_line(symbol, title, Color::Cyan)
            }
            HistoryNodeDisplay::Resource(ResourceDisplay::Info { label, color }) => {
                let symbol = connector.unwrap_or("└─");
                Self::resource_line(symbol, label, *color)
            }
            HistoryNodeDisplay::FileEntry(entry) => {
                let symbol = connector.unwrap_or("└─");
                Self::file_entry_line(symbol, &entry.name, entry.is_dir)
            }
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

    fn run_header_line(display: &RunHeaderDisplay, collapsed: bool) -> Line<'static> {
        let mut spans = Vec::new();
        spans.push(Span::styled(
            format!("{} ", if collapsed { "▸" } else { "▾" }),
            Style::default().fg(Color::DarkGray),
        ));
        let label = match &display.kind {
            RunHeaderKind::Current => format!("{} (active)", display.run_id),
            RunHeaderKind::Finished { finished_at } => {
                format!("{} ({finished_at})", display.run_id)
            }
        };
        spans.push(Span::styled(
            label,
            Self::history_status_style(display.status),
        ));
        if display.viewing {
            spans.push(Span::styled(
                " [viewing]".to_string(),
                Style::default().fg(Color::Yellow),
            ));
        }
        Line::from(spans)
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
            .bg(Color::Rgb(36, 48, 74))
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        for span in &mut line.spans {
            span.style = span.style.patch(highlight);
        }
        line
    }

    fn pane_block<T: Into<ratatui::text::Line<'static>>>(
        &self,
        title: T,
        focused: bool,
    ) -> Block<'static> {
        let border_style = if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let title_style = if focused {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style)
            .title(title)
            .title_style(title_style)
    }

    pub(super) fn is_run_collapsed(&self, key: &str) -> bool {
        let map_key = Self::run_collapse_key(key);
        let default = key != CURRENT_HISTORY_KEY;
        self.collapsed_nodes
            .get(&map_key)
            .copied()
            .unwrap_or(default)
    }

    pub(super) fn set_run_collapsed(&mut self, key: &str, collapsed: bool) {
        let map_key = Self::run_collapse_key(key);
        self.collapsed_nodes.insert(map_key, collapsed);
    }

    pub(super) fn history_pane_visible(&self) -> bool {
        self.show_history_pane
    }

    pub(super) fn job_yaml_pane_visible(&self) -> bool {
        self.show_job_yaml_pane
    }

    pub(super) fn toggle_history_pane(&mut self) {
        self.show_history_pane = !self.show_history_pane;
        if !self.show_history_pane && self.focus == PaneFocus::History {
            self.focus = PaneFocus::Jobs;
        }
    }

    pub(super) fn toggle_job_yaml_pane(&mut self) {
        self.show_job_yaml_pane = !self.show_job_yaml_pane;
        if !self.show_job_yaml_pane && self.focus == PaneFocus::JobYaml {
            self.focus = PaneFocus::Jobs;
        }
        if self.show_job_yaml_pane {
            self.job_yaml_scroll = 0;
        }
    }

    pub(super) fn toggle_focus(&mut self) {
        let mut panes = Vec::new();
        if self.show_history_pane {
            panes.push(PaneFocus::History);
        }
        panes.push(PaneFocus::Jobs);
        if self.show_job_yaml_pane {
            panes.push(PaneFocus::JobYaml);
        }
        let current = panes
            .iter()
            .position(|pane| *pane == self.focus)
            .unwrap_or(0);
        self.focus = panes[(current + 1) % panes.len()];
        if self.focus_is_history() {
            self.history_selection = self
                .history_selection
                .min(self.history_nodes().len().saturating_sub(1));
        }
    }

    pub(super) fn focus_is_history(&self) -> bool {
        matches!(self.focus, PaneFocus::History)
    }

    pub(super) fn focus_is_job_yaml(&self) -> bool {
        matches!(self.focus, PaneFocus::JobYaml)
    }

    fn focus_is_jobs(&self) -> bool {
        matches!(self.focus, PaneFocus::Jobs)
    }

    pub(super) fn push_history_entry(&mut self, entry: HistoryEntry) {
        self.collapsed_nodes
            .entry(Self::run_collapse_key(&entry.run_id))
            .or_insert(true);
        self.history.push(entry);
        self.loaded_dirs.clear();
    }

    pub(super) fn view_history_run(&mut self, run_id: &str) -> Result<()> {
        if self.has_current_run && run_id == self.current_run_id {
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
            .map(|job| UiJobState::from_history(run_id, job, &self.workdir))
            .collect();
        self.history_view = Some(HistoryRunView {
            run_id: run_id.to_string(),
            jobs,
            selected: 0,
        });
        self.focus = PaneFocus::Jobs;
        self.on_active_selection_changed();
        self.loaded_dirs.clear();
        Ok(())
    }

    pub(super) fn view_history_job(&mut self, run_id: &str, job_name: &str) -> Result<()> {
        self.view_history_run(run_id)?;
        if let Some(view) = &mut self.history_view
            && let Some(idx) = view.jobs.iter().position(|job| job.name == job_name)
        {
            view.selected = idx;
            self.on_active_selection_changed();
        }
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
        self.loaded_dirs.clear();
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
            HistoryNodeKey::ResourceDir { .. } => {
                if !self.is_node_collapsed_key(&nodes[idx].key) {
                    self.set_node_collapsed_key(&nodes[idx].key, true);
                } else if let Some(parent) = nodes[idx].parent_index {
                    self.history_selection = parent;
                }
            }
            HistoryNodeKey::FileEntry { is_dir, .. } if *is_dir => {
                if !self.is_node_collapsed_key(&nodes[idx].key) {
                    self.set_node_collapsed_key(&nodes[idx].key, true);
                } else if let Some(parent) = nodes[idx].parent_index {
                    self.history_selection = parent;
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
            HistoryNodeKey::ResourceDir { path, .. } => {
                if self.is_node_collapsed_key(&nodes[idx].key) {
                    self.ensure_dir_loaded(path);
                    self.set_node_collapsed_key(&nodes[idx].key, false);
                } else if let Some(next) = nodes.get(idx + 1)
                    && next.parent_index == Some(idx)
                {
                    self.history_selection = idx + 1;
                }
            }
            HistoryNodeKey::FileEntry { path, is_dir } if *is_dir => {
                if self.is_node_collapsed_key(&nodes[idx].key) {
                    self.ensure_dir_loaded(path);
                    self.set_node_collapsed_key(&nodes[idx].key, false);
                } else if let Some(next) = nodes.get(idx + 1)
                    && next.parent_index == Some(idx)
                {
                    self.history_selection = idx + 1;
                }
            }
            HistoryNodeKey::ResourceInfo => {
                if let HistoryNodeDisplay::Resource(ResourceDisplay::Info { label, .. }) =
                    &nodes[idx].display
                {
                    self.load_text_preview("Runtime details".to_string(), label.clone());
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
                run_id, job_name, ..
            } => Some(HistoryAction::ViewHistoryJob {
                run_id: run_id.clone(),
                job_name: job_name.clone(),
            }),
            HistoryNodeKey::ResourceDir { title, path } => {
                self.ensure_dir_loaded(path);
                Some(HistoryAction::ViewDir {
                    title: title.clone(),
                    path: path.clone(),
                })
            }
            HistoryNodeKey::FileEntry { path, is_dir } => {
                let title = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                if *is_dir {
                    self.ensure_dir_loaded(path);
                    Some(HistoryAction::ViewDir {
                        title,
                        path: path.clone(),
                    })
                } else {
                    Some(HistoryAction::ViewFile {
                        title,
                        path: path.clone(),
                    })
                }
            }
            HistoryNodeKey::ResourceInfo => {
                if let HistoryNodeDisplay::Resource(ResourceDisplay::Info { label, .. }) =
                    &nodes[idx].display
                {
                    self.load_text_preview("Runtime details".to_string(), label.clone());
                }
                None
            }
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

    pub(super) fn load_text_preview(&mut self, title: String, text: String) {
        self.history_preview = Some(HistoryPreview {
            title,
            path: PathBuf::new(),
            lines: text.lines().map(str::to_string).collect(),
            scroll_offset: 0,
        });
        self.focus = PaneFocus::Jobs;
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
        let compact = self.use_compact_tabs(available);

        let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut width = 0usize;

        for (idx, job) in jobs.iter().enumerate() {
            let label_spans =
                self.build_label_spans(job, idx == self.active_selected_index(), compact);
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

    pub(super) fn build_label_spans(
        &self,
        job: &UiJobState,
        selected: bool,
        compact: bool,
    ) -> Vec<Span<'static>> {
        let (icon_char, icon_color) = job.status.icon();
        let active = job.status == UiJobStatus::Running;
        let overlay = if selected {
            Some(
                Style::default()
                    .bg(if active {
                        Color::Cyan
                    } else {
                        Color::Rgb(36, 48, 74)
                    })
                    .fg(if active { Color::Black } else { Color::White })
                    .add_modifier(Modifier::BOLD),
            )
        } else if active {
            Some(Style::default().fg(Color::Cyan))
        } else {
            None
        };
        let mut spans = Vec::new();

        spans.push(Span::styled(
            icon_char.to_string(),
            Self::apply_highlight(
                Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
                overlay,
            ),
        ));
        spans.push(Span::styled(
            " ".to_string(),
            Self::apply_highlight(Style::default(), overlay),
        ));
        if compact {
            spans.push(Span::styled(
                truncate_label(&job.name, 18),
                Self::apply_highlight(Style::default().add_modifier(Modifier::BOLD), overlay),
            ));
        } else {
            spans.push(Span::styled(
                job.name.clone(),
                Self::apply_highlight(Style::default().add_modifier(Modifier::BOLD), overlay),
            ));
            spans.push(Span::styled(
                format!(" · {}", job.stage),
                Self::apply_highlight(Style::default().fg(Color::Gray), overlay),
            ));
            spans.push(Span::styled(
                format!(" · {}", job.status.label()),
                Self::apply_highlight(Style::default().fg(icon_color), overlay),
            ));
        }
        if job.manual_pending {
            spans.push(Span::styled(
                if compact { " !" } else { "  manual" },
                Self::apply_highlight(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                    overlay,
                ),
            ));
        } else if job.status == UiJobStatus::Pending {
            spans.push(Span::styled(
                if compact { " …" } else { "  waiting" },
                Self::apply_highlight(Style::default().fg(Color::DarkGray), overlay),
            ));
        } else if active {
            spans.push(Span::styled(
                if compact { " *" } else { "  active" },
                Self::apply_highlight(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                    overlay,
                ),
            ));
        }
        if job.analysis_running {
            spans.push(Span::styled(
                if compact { " ai…" } else { "  ai…" },
                Self::apply_highlight(
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                    overlay,
                ),
            ));
        } else if !job.analysis_text.is_empty() || job.analysis_error.is_some() {
            spans.push(Span::styled(
                if compact { " ai" } else { "  ai" },
                Self::apply_highlight(Style::default().fg(Color::Magenta), overlay),
            ));
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

    pub(super) fn help_prompt(&self, width: u16) -> Paragraph<'static> {
        let lines = if self.focus_is_history() {
            vec![
                Line::from(vec![
                    Self::hint_label("History", Color::Cyan),
                    key_span_color("↑/↓", Color::Yellow),
                    Span::raw(" move   │   "),
                    key_span_color("←/→", Color::Yellow),
                    Span::raw(" fold   │   "),
                    key_span_color("Enter", Color::Yellow),
                    Span::raw(" open   │   "),
                    key_span_color("o", Color::Yellow),
                    Span::raw(" pager   │   "),
                    key_span_color("Tab", Color::Yellow),
                    Span::raw(" focus"),
                ]),
                Line::from(vec![
                    Self::hint_label("Panes", Color::Green),
                    key_span_color("H", Color::Yellow),
                    Span::raw(" history   │   "),
                    key_span_color("Y", Color::Yellow),
                    Span::raw(" yaml   │   "),
                    key_span_color("?", Color::Yellow),
                    Span::raw(" docs   │   "),
                    key_span_color("q", Color::Yellow),
                    Span::raw(" quit"),
                ]),
            ]
        } else if self.focus_is_job_yaml() {
            vec![
                Line::from(vec![
                    Self::hint_label("YAML", Color::Cyan),
                    key_span_color("↑/↓", Color::Yellow),
                    Span::raw(" scroll   │   "),
                    key_span_color("PgUp/PgDn", Color::Yellow),
                    Span::raw(" page   │   "),
                    key_span_color("y", Color::Yellow),
                    Span::raw(" pager   │   "),
                    key_span_color("H", Color::Yellow),
                    Span::raw(" history"),
                ]),
                Line::from(vec![
                    Self::hint_label("Panes", Color::Green),
                    key_span_color("Y", Color::Yellow),
                    Span::raw(" yaml   │   "),
                    key_span_color("Tab", Color::Yellow),
                    Span::raw(" focus   │   "),
                    key_span_color("?", Color::Yellow),
                    Span::raw(" docs   │   "),
                    key_span_color("q", Color::Yellow),
                    Span::raw(" quit"),
                ]),
            ]
        } else {
            vec![
                Line::from(vec![
                    Self::hint_label("Jobs", Color::Cyan),
                    key_span_color("j/k", Color::Yellow),
                    Span::raw(" jobs   │   "),
                    key_span_color("o", Color::Yellow),
                    Span::raw(" log   │   "),
                    key_span_color("y", Color::Yellow),
                    Span::raw(" yaml   │   "),
                    key_span_color("p", Color::Yellow),
                    Span::raw(" plan   │   "),
                    key_span_color("a", Color::Yellow),
                    Span::raw(" analyze   │   "),
                    key_span_color("A", Color::Yellow),
                    Span::raw(" prompt"),
                ]),
                Line::from(vec![
                    Self::hint_label("View", Color::Magenta),
                    key_span_color("r", Color::Yellow),
                    Span::raw(" restart   │   "),
                    key_span_color("m", Color::Yellow),
                    Span::raw(" manual   │   "),
                    key_span_color("x", Color::Yellow),
                    Span::raw(" cancel   │   "),
                    key_span_color("?", Color::Yellow),
                    Span::raw(" docs   │   "),
                    key_span_color("H/Y", Color::Yellow),
                    Span::raw(" panes   │   "),
                    key_span_color("Tab", Color::Yellow),
                    Span::raw(" focus   │   "),
                    key_span_color("0-4", Color::Yellow),
                    Span::raw(format!(" {}   │   ", self.log_filter.label())),
                    key_span_color("c", Color::Yellow),
                    Span::raw(format!(" {}   │   ", self.tab_density.label())),
                    key_span_color("q", Color::Yellow),
                    Span::raw(" quit"),
                ]),
            ]
        };
        let lines = self.with_footer_brand(lines, width.saturating_sub(2) as usize);
        Paragraph::new(lines)
            .block(self.pane_block(
                "Shortcuts",
                self.focus_is_history() || self.focus_is_job_yaml(),
            ))
            .wrap(Wrap { trim: false })
    }

    fn with_footer_brand(&self, mut lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
        let brand = [
            "opal.cloudflavor.io",
            concat!("opal ", env!("CARGO_PKG_VERSION")),
        ];
        for (idx, text) in brand.into_iter().enumerate() {
            if let Some(line) = lines.get_mut(idx) {
                let pad = width.saturating_sub(line.width() + text.len());
                if pad > 0 {
                    line.spans.push(Span::raw(" ".repeat(pad)));
                } else {
                    line.spans.push(Span::raw("  "));
                }
                line.spans.push(Span::styled(
                    text.to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
        lines
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

    fn file_entry_line(connector: &str, name: &str, is_dir: bool) -> Line<'static> {
        let color = if is_dir { Color::Cyan } else { Color::Gray };
        Line::from(vec![
            Span::styled(
                format!("     {} ", connector),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(name.to_string(), Style::default().fg(color)),
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
            spans.push(Span::raw(" to switch docs/shortcuts"));
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
                if idx + 1 >= self.help_docs.len() {
                    self.show_help_shortcuts();
                } else {
                    self.open_help_document(idx + 1);
                }
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
                if idx == 0 {
                    self.show_help_shortcuts();
                } else {
                    self.open_help_document(idx - 1);
                }
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
                    ("c", "cycle lane density"),
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
                    ("0-4", "log filters"),
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
                    ("H", "hide/show history"),
                    ("Y", "hide/show YAML"),
                    ("Tab", "switch panes"),
                    ("q", "quit"),
                ],
            ),
            Self::help_section(
                "Plan/YAML/AI",
                Color::White,
                &[
                    ("p", "open plan in pager"),
                    ("y", "open job YAML in pager"),
                    ("a", "analyze selected job"),
                    ("A", "preview rendered AI prompt"),
                ],
            ),
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

    pub(super) fn info_panel_height(&self) -> u16 {
        let extra = self
            .active_job()
            .map(|job| u16::from(job.manual_pending))
            .unwrap_or(0);
        4 + extra
    }

    pub(super) fn info_panel(&self) -> Paragraph<'_> {
        let summary = self.pipeline_counts();
        let job = match self.active_job() {
            Some(job) => job,
            None => {
                return Paragraph::new(vec![Line::from("No job selected")])
                    .block(self.pane_block("Details", self.focus_is_jobs()))
                    .wrap(Wrap { trim: true });
            }
        };
        let log_name = job
            .log_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| job.log_path.display().to_string());
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Job: ", Style::default().fg(Color::Cyan)),
                Span::raw(job.name.clone()),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Stage: ", Style::default().fg(Color::Cyan)),
                Span::raw(job.stage.clone()),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Status: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!("{} ({:.2}s)", job.status.label(), job.display_duration()),
                    Style::default().fg(job.status.icon().1),
                ),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("AI: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    job.analysis_provider.as_deref().unwrap_or("none"),
                    Style::default().fg(Color::Magenta),
                ),
                Span::styled(" ", Style::default()),
                Span::styled(
                    if job.analysis_running {
                        "running"
                    } else if job.analysis_error.is_some() {
                        "error"
                    } else if !job.analysis_text.is_empty() {
                        "ready"
                    } else {
                        "idle"
                    },
                    Style::default().fg(if job.analysis_running {
                        Color::Magenta
                    } else if job.analysis_error.is_some() {
                        Color::Red
                    } else if !job.analysis_text.is_empty() {
                        Color::Green
                    } else {
                        Color::DarkGray
                    }),
                ),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Log: ", Style::default().fg(Color::Cyan)),
                Span::raw(log_name),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("id ", Style::default().fg(Color::DarkGray)),
                Span::styled(job.log_hash.clone(), Style::default().fg(Color::DarkGray)),
            ]),
            Line::from(vec![
                Span::styled("Runner: ", Style::default().fg(Color::Cyan)),
                Span::raw(job.runner.engine.clone()),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Arch: ", Style::default().fg(Color::Cyan)),
                Span::raw(
                    job.runner
                        .arch
                        .clone()
                        .unwrap_or_else(|| "native/default".to_string()),
                ),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("vCPU: ", Style::default().fg(Color::Cyan)),
                Span::raw(
                    job.runner
                        .cpus
                        .clone()
                        .unwrap_or_else(|| "engine default".to_string()),
                ),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("RAM: ", Style::default().fg(Color::Cyan)),
                Span::raw(
                    job.runner
                        .memory
                        .clone()
                        .unwrap_or_else(|| "engine default".to_string()),
                ),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("Progress: ", Style::default().fg(Color::Cyan)),
                Span::raw(format!("{}/{}", summary.done, summary.total)),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("running ", Style::default().fg(Color::Cyan)),
                Span::raw(summary.running.to_string()),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("failed ", Style::default().fg(Color::Cyan)),
                Span::raw(summary.failed.to_string()),
                Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
                Span::styled("pending ", Style::default().fg(Color::Cyan)),
                Span::raw(summary.pending.to_string()),
            ]),
        ];
        if job.manual_pending {
            lines.push(Line::from(vec![
                Span::styled("Manual: ", Style::default().fg(Color::Yellow)),
                Span::raw("waiting (press 'm' to start)"),
            ]));
        }

        Paragraph::new(lines)
            .block(self.pane_block("Details", self.focus_is_jobs()))
            .wrap(Wrap { trim: true })
    }

    pub(super) fn job_yaml_panel(&self) -> Paragraph<'_> {
        let (title, content) = self.active_job_yaml_text();
        let mut paragraph = Paragraph::new(text_lines(&content))
            .block(self.pane_block(title, self.focus_is_job_yaml()))
            .wrap(Wrap { trim: false });
        if self.job_yaml_scroll > 0 {
            paragraph = paragraph.scroll((self.job_yaml_scroll, 0));
        }
        paragraph
    }

    pub(super) fn job_yaml_text_for_pager(&self) -> String {
        self.active_job_yaml_text().1
    }

    pub(super) fn scroll_job_yaml_line_up(&mut self) {
        self.job_yaml_scroll = self.job_yaml_scroll.saturating_sub(1);
    }

    pub(super) fn scroll_job_yaml_line_down(&mut self) {
        self.job_yaml_scroll = self.job_yaml_scroll.saturating_add(1);
    }

    pub(super) fn scroll_job_yaml_page_up(&mut self, rows: u16) {
        self.job_yaml_scroll = self.job_yaml_scroll.saturating_sub(rows.max(1));
    }

    pub(super) fn scroll_job_yaml_page_down(&mut self, rows: u16) {
        self.job_yaml_scroll = self.job_yaml_scroll.saturating_add(rows.max(1));
    }

    fn active_job_yaml_text(&self) -> (String, String) {
        let Some(job) = self.active_job() else {
            return ("Job YAML".to_string(), "No job selected".to_string());
        };
        let source_name = &job.source_name;
        let title = if source_name == &job.name {
            format!("Job YAML • {}", job.name)
        } else {
            format!("Job YAML • {} ← {}", job.name, source_name)
        };
        if let Some(content) = self.job_yaml_map.get(source_name) {
            return (title, content.clone());
        }
        if let Some(content) = self.job_yaml_map.get(&job.name) {
            return (title, content.clone());
        }
        if let Some(err) = &self.job_yaml_error {
            return (title, format!("failed to load job YAML: {err}"));
        }
        (
            title,
            yaml_source_hint(&job.name, source_name, &self.pipeline_path),
        )
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
                    .block(self.pane_block("Logs", self.focus_is_jobs()))
                    .wrap(Wrap { trim: true });
            }
        };
        let mut lines: Vec<Line> = Vec::new();
        let status_span = if job.show_analysis {
            let provider = job.analysis_provider.as_deref().unwrap_or("analysis");
            let state = if job.analysis_running {
                "running"
            } else if job.analysis_error.is_some() {
                "failed"
            } else {
                "complete"
            };
            Span::styled(
                format!(
                    "{}  {}/{} done  provider:{}",
                    state,
                    self.pipeline_counts().done,
                    self.pipeline_counts().total,
                    provider,
                ),
                Style::default().fg(if job.analysis_error.is_some() {
                    Color::Red
                } else if job.analysis_running {
                    Color::Yellow
                } else {
                    Color::Green
                }),
            )
        } else {
            Span::styled(
                format!(
                    "{}  {}/{} done  filter:{}",
                    job.status.label(),
                    self.pipeline_counts().done,
                    self.pipeline_counts().total,
                    self.log_filter.label(),
                ),
                Style::default().fg(job.status.icon().1),
            )
        };
        lines.push(Line::from(status_span));
        if job.show_analysis {
            if let Some(err) = &job.analysis_error {
                lines.push(Line::from(Span::styled(
                    format!("Error: {}", err),
                    Style::default().fg(Color::Red),
                )));
            } else if let Some(path) = &job.analysis_saved_path {
                lines.push(Line::from(Span::styled(
                    format!("Saved: {}", path.display()),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        } else if let Some(err) = &job.error {
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
        if job.show_analysis {
            lines.extend(job.visible_analysis(inner_width, available_rows));
        } else {
            lines.extend(job.visible_logs(inner_width, available_rows, self.log_filter));
        }

        let mut title = if job.show_analysis {
            "AI Analysis".to_string()
        } else {
            "Logs".to_string()
        };
        if job.show_analysis {
            if job.analysis_running {
                title.push_str(" (running)");
            } else {
                title.push_str(" (complete)");
            }
        } else {
            if job.status.is_done() {
                title.push_str(" (complete)");
            }
            if self.log_filter != LogFilter::All {
                title.push_str(&format!(" [{}]", self.log_filter.label()));
            }
        }

        Paragraph::new(lines)
            .block(self.pane_block(title, self.focus_is_jobs()))
            .wrap(Wrap { trim: false })
    }

    pub(super) fn current_log_path(&self) -> Option<PathBuf> {
        if self.active_job().is_some_and(|job| job.show_analysis) {
            return None;
        }
        self.active_job().and_then(|job| {
            if job.log_path.exists() {
                Some(job.log_path.clone())
            } else {
                None
            }
        })
    }

    pub(super) fn current_analysis_text(&self) -> Option<String> {
        let job = self.active_job()?;
        if !job.show_analysis {
            return None;
        }
        let mut text = String::new();
        if let Some(provider) = &job.analysis_provider {
            text.push_str(&format!("provider: {provider}\n\n"));
        }
        text.push_str(&job.analysis_text);
        if let Some(error) = &job.analysis_error {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str("error: ");
            text.push_str(error);
        }
        Some(text)
    }

    pub(super) fn ai_prompt_preview_request(&self) -> Option<(String, String)> {
        let job = self.jobs.get(self.selected)?;
        Some((job.name.clone(), job.source_name.clone()))
    }

    pub(super) fn analysis_snapshot(&self) -> Option<AiAnalysisSnapshot> {
        let job = self.active_job()?;
        let run_id = self
            .history_view
            .as_ref()
            .map(|view| view.run_id.clone())
            .unwrap_or_else(|| self.current_run_id.clone());

        let runtime_summary_path = if let Some(view) = &self.history_view {
            self.history
                .iter()
                .find(|entry| entry.run_id == view.run_id)
                .and_then(|entry| {
                    entry
                        .jobs
                        .iter()
                        .find(|history_job| history_job.name == job.name)
                })
                .and_then(|history_job| history_job.runtime_summary_path.clone())
        } else {
            self.job_resources
                .get(&job.name)
                .and_then(|resources| resources.runtime_summary_path.clone())
        };

        let runtime_summary = runtime_summary_path
            .as_deref()
            .and_then(|path| fs::read_to_string(path).ok());
        let log_excerpt = fs::read_to_string(&job.log_path)
            .ok()
            .map(|content| {
                let lines: Vec<&str> = content.lines().collect();
                let start = lines.len().saturating_sub(200);
                lines[start..].join("\n")
            })
            .unwrap_or_default();
        let pipeline_summary = if self.plan_text.is_empty() {
            "plan unavailable in this view".to_string()
        } else {
            self.plan_text.clone()
        };
        let runner_summary = format!(
            "engine={} arch={} vcpu={} ram={}",
            job.runner.engine,
            job.runner
                .arch
                .clone()
                .unwrap_or_else(|| "native/default".to_string()),
            job.runner
                .cpus
                .clone()
                .unwrap_or_else(|| "engine default".to_string()),
            job.runner
                .memory
                .clone()
                .unwrap_or_else(|| "engine default".to_string())
        );
        let (_, job_yaml) = self.active_job_yaml_text();

        Some(AiAnalysisSnapshot {
            run_id,
            job_name: job.name.clone(),
            source_name: job.source_name.clone(),
            stage: job.stage.clone(),
            job_yaml,
            runner_summary,
            pipeline_summary,
            runtime_summary,
            log_excerpt,
            failure_hint: job.error.clone(),
        })
    }

    pub(super) fn toggle_ai_prompt_preview(&mut self) -> bool {
        let Some(preview) = &self.history_preview else {
            return false;
        };
        if preview.title.starts_with("AI Prompt • ") {
            self.clear_history_preview();
            true
        } else {
            false
        }
    }

    pub(super) fn analysis_action_request(&mut self) -> Option<(String, String)> {
        if self.history_view.is_some() {
            return None;
        }
        let job = self.jobs.get_mut(self.selected)?;
        if job.analysis_running || !job.analysis_text.is_empty() || job.analysis_error.is_some() {
            job.show_analysis = !job.show_analysis;
            job.scroll_offset = 0;
            job.follow_logs = true;
            return None;
        }
        job.start_analysis("ollama".to_string());
        Some((job.name.clone(), job.source_name.clone()))
    }

    pub(super) fn analysis_started(&mut self, name: &str, provider: &str) {
        if let Some(job) = self.job_by_name_mut(name) {
            job.start_analysis(provider.to_string());
        }
    }

    pub(super) fn analysis_chunk(&mut self, name: &str, delta: &str) {
        if let Some(job) = self.job_by_name_mut(name) {
            job.append_analysis(delta);
        }
    }

    pub(super) fn analysis_finished(
        &mut self,
        name: &str,
        final_text: String,
        saved_path: Option<PathBuf>,
        error: Option<String>,
    ) {
        if let Some(job) = self.job_by_name_mut(name) {
            job.finish_analysis(final_text, saved_path, error);
        }
    }

    pub(super) fn ai_prompt_ready(&mut self, name: &str, prompt: String) {
        if self.active_job().is_some_and(|job| job.name == name) {
            self.load_text_preview(format!("AI Prompt • {name}"), prompt);
            self.scroll_history_preview_to_top();
        }
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

    fn job_by_name_mut(&mut self, name: &str) -> Option<&mut UiJobState> {
        self.jobs.iter_mut().find(|job| job.name == name)
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

    pub(super) fn set_log_filter(&mut self, filter: LogFilter) {
        self.log_filter = filter;
        if let Some(job) = self.active_job_mut() {
            job.auto_follow();
        }
    }

    pub(super) fn cycle_tab_density(&mut self) {
        self.tab_density = match self.tab_density {
            TabDensity::Auto => TabDensity::Compact,
            TabDensity::Compact => TabDensity::Full,
            TabDensity::Full => TabDensity::Auto,
        };
    }

    fn use_compact_tabs(&self, available: usize) -> bool {
        match self.tab_density {
            TabDensity::Compact => true,
            TabDensity::Full => false,
            TabDensity::Auto => {
                let jobs = self.active_jobs().len();
                jobs >= 8 || available < jobs.saturating_mul(24)
            }
        }
    }

    fn pipeline_counts(&self) -> PipelineCounts {
        let mut counts = PipelineCounts {
            total: self.active_jobs().len(),
            ..PipelineCounts::default()
        };
        for job in self.active_jobs() {
            match job.status {
                UiJobStatus::Running => counts.running += 1,
                UiJobStatus::Pending => counts.pending += 1,
                UiJobStatus::Success => counts.success += 1,
                UiJobStatus::Failed => counts.failed += 1,
                UiJobStatus::Skipped => counts.skipped += 1,
            }
        }
        counts.done = counts.success + counts.failed + counts.skipped;
        counts
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

#[derive(Default)]
struct PipelineCounts {
    total: usize,
    done: usize,
    running: usize,
    pending: usize,
    success: usize,
    failed: usize,
    skipped: usize,
}

fn truncate_label(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn matches_log_filter(line: &str, filter: LogFilter) -> bool {
    if filter == LogFilter::All {
        return true;
    }
    let content = strip_log_metadata(line).to_ascii_lowercase();
    match filter {
        LogFilter::All => true,
        LogFilter::Errors => {
            content.contains("error")
                || content.contains("failed")
                || content.contains("panic")
                || content.contains("exception")
        }
        LogFilter::Warnings => content.contains("warning") || content.contains("warn"),
        LogFilter::Downloads => {
            content.contains("downloaded")
                || content.contains("downloading")
                || content.contains("fetching")
        }
        LogFilter::Build => {
            content.contains("compiling")
                || content.contains("checking")
                || content.contains("building")
                || content.contains("running")
                || content.contains("linking")
                || content.contains("finished")
        }
    }
}

fn strip_log_metadata(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('[')
        && let Some(idx) = rest.find("] ")
    {
        return &rest[idx + 2..];
    }
    line
}

pub(super) struct UiJobState {
    name: String,
    source_name: String,
    stage: String,
    log_path: PathBuf,
    log_hash: String,
    runner: UiRunnerInfo,
    status: UiJobStatus,
    duration: f32,
    start_time: Option<Instant>,
    error: Option<String>,
    logs: Vec<String>,
    analysis_text: String,
    analysis_provider: Option<String>,
    analysis_error: Option<String>,
    analysis_saved_path: Option<PathBuf>,
    analysis_running: bool,
    show_analysis: bool,
    scroll_offset: usize,
    follow_logs: bool,
    manual_pending: bool,
}

impl UiJobState {
    fn from(info: UiJobInfo) -> Self {
        Self {
            name: info.name,
            source_name: info.source_name,
            stage: info.stage,
            log_path: info.log_path,
            log_hash: info.log_hash,
            runner: info.runner,
            status: UiJobStatus::Pending,
            duration: 0.0,
            start_time: None,
            error: None,
            logs: Vec::new(),
            analysis_text: String::new(),
            analysis_provider: None,
            analysis_error: None,
            analysis_saved_path: None,
            analysis_running: false,
            show_analysis: false,
            scroll_offset: 0,
            follow_logs: true,
            manual_pending: false,
        }
    }

    fn from_history(run_id: &str, job: &HistoryJob, _workdir: &Path) -> Self {
        let log_path = job
            .log_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| runtime::logs_dir(run_id).join(format!("{}.log", job.log_hash)));
        Self {
            name: job.name.clone(),
            source_name: job.name.clone(),
            stage: job.stage.clone(),
            log_path,
            log_hash: job.log_hash.clone(),
            runner: UiRunnerInfo::default(),
            status: UiJobStatus::from_history(job.status),
            duration: 0.0,
            start_time: None,
            error: None,
            logs: Vec::new(),
            analysis_text: String::new(),
            analysis_provider: None,
            analysis_error: None,
            analysis_saved_path: None,
            analysis_running: false,
            show_analysis: false,
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

    fn start_analysis(&mut self, provider: String) {
        self.analysis_text.clear();
        self.analysis_provider = Some(provider);
        self.analysis_error = None;
        self.analysis_saved_path = None;
        self.analysis_running = true;
        self.show_analysis = true;
        self.scroll_offset = 0;
        self.follow_logs = true;
    }

    fn append_analysis(&mut self, delta: &str) {
        self.analysis_text.push_str(delta);
        if self.follow_logs {
            self.scroll_offset = 0;
        }
    }

    fn finish_analysis(
        &mut self,
        final_text: String,
        saved_path: Option<PathBuf>,
        error: Option<String>,
    ) {
        if !final_text.trim().is_empty() {
            self.analysis_text = final_text;
        }
        self.analysis_saved_path = saved_path;
        self.analysis_error = error;
        self.analysis_running = false;
        self.show_analysis = true;
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
        self.analysis_text.clear();
        self.analysis_provider = None;
        self.analysis_error = None;
        self.analysis_saved_path = None;
        self.analysis_running = false;
        self.show_analysis = false;
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
        let line_count = if self.show_analysis {
            self.analysis_text.lines().count()
        } else {
            self.logs.len()
        };
        if line_count == 0 || lines == 0 {
            return;
        }
        match direction {
            ScrollDirection::Up => {
                let max_scroll = line_count.saturating_sub(1);
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
        let line_count = if self.show_analysis {
            self.analysis_text.lines().count()
        } else {
            self.logs.len()
        };
        self.scroll_offset = line_count.saturating_sub(1);
        if self.scroll_offset > 0 {
            self.follow_logs = false;
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.follow_logs = true;
    }

    fn visible_logs(
        &self,
        wrap_width: usize,
        max_rows: usize,
        filter: LogFilter,
    ) -> Vec<Line<'static>> {
        let filtered: Vec<&str> = self
            .logs
            .iter()
            .map(String::as_str)
            .filter(|line| matches_log_filter(line, filter))
            .collect();

        if filtered.is_empty() {
            if self.logs.is_empty() {
                return vec![Line::from("(no output yet)")];
            }
            return vec![Line::from("(no lines match current filter)")];
        }
        if self.logs.is_empty() {
            return vec![Line::from("(no output yet)")];
        }

        let wrap_width = wrap_width.max(1);
        let mut remaining_rows = max_rows.max(1);
        let total = filtered.len();
        let offset = self.scroll_offset.min(total.saturating_sub(1));
        let mut end = total.saturating_sub(offset);
        if end == 0 {
            end = total;
        }

        let mut collected: Vec<Line<'static>> = Vec::new();
        while end > 0 {
            let idx = end - 1;
            let line = format_log_entry(filtered[idx]);
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

    fn visible_analysis(&self, wrap_width: usize, max_rows: usize) -> Vec<Line<'static>> {
        let lines: Vec<&str> = self.analysis_text.lines().collect();
        if lines.is_empty() {
            if self.analysis_running {
                return vec![Line::from("(waiting for model output)")];
            }
            if self.analysis_error.is_some() {
                return vec![Line::from("(analysis failed)")];
            }
            return vec![Line::from("(no analysis yet)")];
        }

        let wrap_width = wrap_width.max(1);
        let mut remaining_rows = max_rows.max(1);
        let total = lines.len();
        let offset = self.scroll_offset.min(total.saturating_sub(1));
        let mut end = total.saturating_sub(offset);
        if end == 0 {
            end = total;
        }

        let mut collected = Vec::new();
        while end > 0 {
            let idx = end - 1;
            let line = Line::from(lines[idx].to_string());
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
    display: HistoryNodeDisplay,
    line: Line<'static>,
}

#[derive(Clone)]
struct HistoryTreeEntry {
    key: HistoryNodeKey,
    display: HistoryNodeDisplay,
    children: Vec<HistoryTreeEntry>,
    collapsed: bool,
}

#[derive(Clone)]
enum HistoryNodeDisplay {
    RunHeader(RunHeaderDisplay),
    Job(JobDisplay),
    Resource(ResourceDisplay),
    FileEntry(FileEntryDisplay),
}

#[derive(Clone)]
struct RunHeaderDisplay {
    run_id: String,
    status: HistoryStatus,
    kind: RunHeaderKind,
    viewing: bool,
}

#[derive(Clone)]
enum RunHeaderKind {
    Current,
    Finished { finished_at: String },
}

#[derive(Clone)]
struct JobDisplay {
    name: String,
    stage: String,
    hash: String,
    status: HistoryStatus,
}

#[derive(Clone)]
enum ResourceDisplay {
    Directory { title: String },
    Info { label: String, color: Color },
}

#[derive(Clone)]
struct FileEntryDisplay {
    name: String,
    is_dir: bool,
}

#[derive(Clone)]
enum HistoryNodeKey {
    CurrentRun,
    CurrentJob(usize),
    FinishedRun { run_id: String },
    FinishedJob { run_id: String, job_name: String },
    ResourceDir { title: String, path: PathBuf },
    ResourceInfo,
    FileEntry { path: PathBuf, is_dir: bool },
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

impl LogFilter {
    pub(super) fn label(self) -> &'static str {
        match self {
            LogFilter::All => "all",
            LogFilter::Errors => "errors",
            LogFilter::Warnings => "warnings",
            LogFilter::Downloads => "downloads",
            LogFilter::Build => "build",
        }
    }
}

impl TabDensity {
    fn label(self) -> &'static str {
        match self {
            TabDensity::Auto => "auto",
            TabDensity::Compact => "compact",
            TabDensity::Full => "full",
        }
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
                Span::styled(timestamp.to_string(), Style::default().fg(Color::DarkGray)),
                Span::raw(" ".to_string()),
                Span::styled(number.to_string(), Style::default().fg(Color::DarkGray)),
                Span::styled("] ".to_string(), Style::default().fg(Color::DarkGray)),
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
        let parsed = parse_text(contents, Options::default());
        let mut lines = Vec::new();
        for line in parsed.lines {
            match line {
                MarkdownLine::Normal(composite) => {
                    if composite.compounds.is_empty() {
                        lines.push(Line::from(""));
                        continue;
                    }

                    let add_blank_after = matches!(composite.style, CompositeStyle::Header(_));
                    lines.push(Self::markdown_composite_line(&composite));
                    if add_blank_after {
                        lines.push(Line::from(""));
                    }
                }
                MarkdownLine::HorizontalRule => lines.push(Line::from(Span::styled(
                    "────────────────────────",
                    Style::default().fg(Color::DarkGray),
                ))),
                _ => lines.push(Line::from("")),
            }
        }
        if lines.is_empty() {
            lines.push(Line::from("This document is empty."));
        }
        lines
    }

    fn markdown_composite_line(composite: &Composite<'_>) -> Line<'static> {
        let (mut spans, base_style, strip_prefix) = match composite.style {
            CompositeStyle::Header(level) => (
                Vec::new(),
                Self::markdown_header_style(level.into()),
                String::new(),
            ),
            CompositeStyle::Quote => (
                vec![Span::styled(
                    "▌ ",
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                )],
                Style::default().fg(Color::LightBlue),
                String::new(),
            ),
            CompositeStyle::Code => (
                vec![Span::raw("    ")],
                Style::default().fg(Color::Green),
                String::new(),
            ),
            CompositeStyle::ListItem(depth) => (
                vec![
                    Span::raw("  ".repeat(depth.saturating_sub(1) as usize)),
                    bullet(),
                ],
                Style::default(),
                String::new(),
            ),
            CompositeStyle::Paragraph => {
                if let Some((prefix, strip_prefix)) = Self::markdown_list_prefix(composite) {
                    (prefix, Style::default(), strip_prefix)
                } else {
                    (Vec::new(), Style::default(), String::new())
                }
            }
        };

        spans.extend(Self::markdown_compound_spans(
            &composite.compounds,
            base_style,
            strip_prefix,
        ));
        Line::from(spans)
    }

    fn markdown_header_style(level: usize) -> Style {
        match level {
            1 => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            2 => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        }
    }

    fn markdown_list_prefix(composite: &Composite<'_>) -> Option<(Vec<Span<'static>>, String)> {
        let raw = composite
            .compounds
            .iter()
            .map(|compound| compound.src)
            .collect::<String>();
        if raw.starts_with("- ") || raw.starts_with("* ") {
            return Some((vec![Span::raw("  "), bullet()], raw[..2].to_string()));
        }

        let marker_len = raw.chars().take_while(|ch| ch.is_ascii_digit()).count();
        if marker_len > 0 && raw[marker_len..].starts_with(". ") {
            let marker = format!("{}.", &raw[..marker_len]);
            return Some((
                vec![Span::styled(
                    format!("  {marker} "),
                    Style::default().fg(Color::DarkGray),
                )],
                format!("{marker} "),
            ));
        }
        None
    }

    fn markdown_compound_spans(
        compounds: &[Compound<'_>],
        base_style: Style,
        strip_prefix: String,
    ) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        let mut strip_prefix = strip_prefix.as_str();
        for compound in compounds {
            let mut text = compound.src;
            if !strip_prefix.is_empty() {
                if let Some(rest) = text.strip_prefix(strip_prefix) {
                    text = rest;
                    strip_prefix = "";
                } else if strip_prefix.starts_with(text) {
                    strip_prefix = &strip_prefix[text.len()..];
                    continue;
                } else {
                    strip_prefix = "";
                }
            }
            if text.is_empty() {
                continue;
            }

            let mut style = base_style;
            if compound.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if compound.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if compound.code {
                style = style.fg(Color::Green).bg(Color::DarkGray);
            }
            if matches!(base_style.fg, Some(Color::LightBlue)) {
                style = style.add_modifier(Modifier::ITALIC);
            }

            spans.push(Span::styled(text.to_string(), style));
        }
        spans
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

pub(super) fn page_file_with_pager(title: &str, path: &Path) -> Result<()> {
    if is_markdown_path(path) {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read markdown file {}", path.display()))?;
        let rendered = render_markdown_for_pager(&contents);
        return page_titled_text_with_pager(title, &rendered);
    }

    let mut file =
        File::open(path).with_context(|| format!("failed to open file {}", path.display()))?;
    page_raw_file_with_pager(title, path, &mut file)
}

fn page_raw_file_with_pager(title: &str, path: &Path, file: &mut File) -> Result<()> {
    let pager = env::var("PAGER").unwrap_or_else(|_| "less -R".to_string());
    let (cmd, args) = parse_pager_command(&pager);
    let mut child = Command::new(&cmd);
    child.args(&args).stdin(Stdio::piped());
    if let Ok(mut handle) = child.spawn() {
        if let Some(mut stdin) = handle.stdin.take() {
            writeln!(stdin, "==> {title} <==")?;
            stdin.write_all(b"\n")?;
            std::io::copy(file, &mut stdin)?;
        }
        let _ = handle.wait();
        return Ok(());
    }
    let _ = Command::new("cat").arg(path).status();
    Ok(())
}

pub(super) fn page_text_with_pager(content: &str) -> Result<()> {
    page_titled_text_with_pager("", content)
}

fn page_titled_text_with_pager(title: &str, content: &str) -> Result<()> {
    let pager = env::var("PAGER").unwrap_or_else(|_| "less -R".to_string());
    let (cmd, args) = parse_pager_command(&pager);
    let mut child = Command::new(&cmd);
    child.args(&args).stdin(Stdio::piped());
    if let Ok(mut handle) = child.spawn() {
        if let Some(mut stdin) = handle.stdin.take() {
            if !title.is_empty() {
                writeln!(stdin, "==> {title} <==")?;
                stdin.write_all(b"\n")?;
            }
            stdin.write_all(content.as_bytes())?;
        }
        let _ = handle.wait();
        return Ok(());
    }
    if !title.is_empty() {
        print!("==> {title} <==\n\n");
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

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown"))
        .unwrap_or(false)
}

fn render_markdown_for_pager(contents: &str) -> String {
    if contents.trim().is_empty() {
        return "This document is empty.\n".to_string();
    }

    let skin = if crate::terminal::should_use_color() {
        MadSkin::default()
    } else {
        MadSkin::no_style()
    };
    format!("{}", skin.term_text(contents))
}

fn colorize_diff_body(body: &str) -> String {
    if let Some(rest) = body.strip_prefix('+') {
        format!("{}", format!("+{rest}").green())
    } else if let Some(rest) = body.strip_prefix('-') {
        format!("{}", format!("-{rest}").red())
    } else if let Some(kind) = semantic_log_color(body) {
        match kind {
            SemanticLogColor::Error => format!("{}", body.red().bold()),
            SemanticLogColor::Warning => format!("{}", body.yellow().bold()),
            SemanticLogColor::Success => format!("{}", body.green().bold()),
            SemanticLogColor::Skipped => format!("{}", body.yellow().italic()),
        }
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
        semantic_log_color(trimmed).map(|kind| match kind {
            SemanticLogColor::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            SemanticLogColor::Warning => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            SemanticLogColor::Success => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            SemanticLogColor::Skipped => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SemanticLogColor {
    Error,
    Warning,
    Success,
    Skipped,
}

fn semantic_log_color(text: &str) -> Option<SemanticLogColor> {
    let normalized = text.to_ascii_lowercase();
    if normalized.contains("...") {
        let tail = normalized.rsplit("...").next().unwrap_or("").trim();
        if tail == "ok" || tail.starts_with("ok ") {
            return Some(SemanticLogColor::Success);
        }
        if tail == "failed" || tail.starts_with("failed ") {
            return Some(SemanticLogColor::Error);
        }
        if tail == "ignored"
            || tail.starts_with("ignored ")
            || tail == "skipped"
            || tail.starts_with("skipped ")
        {
            return Some(SemanticLogColor::Skipped);
        }
    }
    if normalized.contains("test result: ok")
        || (normalized.contains("passed") && normalized.contains("0 failed"))
        || normalized.starts_with("ok")
        || normalized.contains("success")
    {
        return Some(SemanticLogColor::Success);
    }
    if normalized.contains("error")
        || normalized.contains("failed")
        || normalized.contains("panic")
        || normalized.contains("panicked")
        || normalized.contains("exception")
        || normalized.contains("caused by:")
    {
        return Some(SemanticLogColor::Error);
    }
    if normalized.contains("warning") || normalized.contains("warn") {
        return Some(SemanticLogColor::Warning);
    }
    if normalized.contains("skipped") || normalized.contains("ignored") {
        return Some(SemanticLogColor::Skipped);
    }
    None
}

fn apply_line_style(spans: &mut [Span<'static>], style: Style) {
    for span in spans {
        span.style = span.style.patch(style);
    }
}

fn text_lines(content: &str) -> Vec<Line<'static>> {
    if content.is_empty() {
        return vec![Line::from("(empty)")];
    }
    content
        .lines()
        .map(|line| Line::from(line.to_string()))
        .collect()
}

fn load_job_yaml_map(path: &Path) -> Result<HashMap<String, String>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read pipeline file {}", path.display()))?;
    let yaml: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let Some(mapping) = yaml.as_mapping() else {
        return Ok(HashMap::new());
    };
    let mut jobs = HashMap::new();
    for (key, value) in mapping {
        let Some(name) = key.as_str() else {
            continue;
        };
        let mut root = serde_yaml::Mapping::new();
        root.insert(serde_yaml::Value::String(name.to_string()), value.clone());
        let rendered = serde_yaml::to_string(&serde_yaml::Value::Mapping(root))?;
        jobs.insert(name.to_string(), rendered);
    }
    Ok(jobs)
}

fn yaml_source_hint(job_name: &str, source_name: &str, pipeline_path: &Path) -> String {
    if job_name == source_name {
        format!(
            "job definition for '{}' is not available in {}",
            job_name,
            pipeline_path.display()
        )
    } else {
        format!(
            "job definition for '{}' (source job '{}') is not available in {}",
            job_name,
            source_name,
            pipeline_path.display()
        )
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

#[cfg(test)]
mod tests {
    use super::{
        HelpDocument, HelpView, LogFilter, UiState, format_log_entry, is_markdown_path,
        matches_log_filter, render_markdown_for_pager,
    };
    use crate::history::{HistoryEntry, HistoryJob, HistoryStatus};
    use crate::ui::types::{UiJobInfo, UiRunnerInfo};
    use ratatui::style::{Color, Modifier};
    use std::collections::HashMap;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn renders_basic_markdown_for_pager() {
        let rendered = render_markdown_for_pager("**bold** _italic_ `code`\n\n> quote\n");

        assert!(!rendered.contains("**bold**"));
        assert!(!rendered.contains("`code`"));
        assert!(rendered.contains("bold"));
        assert!(rendered.contains("italic"));
        assert!(rendered.contains("code"));
        assert!(rendered.contains("quote"));
    }

    #[test]
    fn detects_common_markdown_extensions() {
        assert!(is_markdown_path(Path::new("docs/guide.md")));
        assert!(is_markdown_path(Path::new("docs/guide.MARKDOWN")));
        assert!(!is_markdown_path(Path::new("docs/guide.txt")));
    }

    #[test]
    fn help_markdown_lines_render_inline_styles() {
        let lines =
            HelpDocument::markdown_lines("# Title\n\nA **bold** *italic* `code`\n> quote\n");

        assert_eq!(lines[0].spans[0].content.as_ref(), "Title");
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::UNDERLINED)
        );

        let body = lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.as_ref() == "bold")
            })
            .expect("body line");
        let body_text = body
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(body_text, "A bold italic code");

        let bold = body
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "bold")
            .expect("bold span");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));

        let italic = body
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "italic")
            .expect("italic span");
        assert!(italic.style.add_modifier.contains(Modifier::ITALIC));

        let code = body
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "code")
            .expect("code span");
        assert_eq!(code.style.fg, Some(Color::Green));
        assert_eq!(code.style.bg, Some(Color::DarkGray));

        let quote = lines
            .iter()
            .find(|line| {
                line.spans
                    .first()
                    .is_some_and(|span| span.content.as_ref() == "▌ ")
            })
            .expect("quote line");
        assert_eq!(quote.spans[0].content.as_ref(), "▌ ");
        assert_eq!(quote.spans[1].content.as_ref(), "quote");
    }

    #[test]
    fn log_filter_modes_match_expected_lines() {
        assert!(matches_log_filter(
            "[11:45:19.541 0068] Downloaded serde v1.0.228",
            LogFilter::Downloads
        ));
        assert!(matches_log_filter(
            "[11:45:19.541 0068] Compiling opal v0.1.0-rc1",
            LogFilter::Build
        ));
        assert!(matches_log_filter(
            "[11:45:19.541 0068] warning: field is never read",
            LogFilter::Warnings
        ));
        assert!(matches_log_filter(
            "[11:45:19.541 0068] error[E0425]: cannot find value",
            LogFilter::Errors
        ));
        assert!(!matches_log_filter(
            "[11:45:19.541 0068] Downloaded serde v1.0.228",
            LogFilter::Errors
        ));
    }

    #[test]
    fn format_log_entry_dims_metadata_prefix() {
        let line = format_log_entry("[11:45:19.541 0068] Downloaded serde");
        assert_eq!(line.spans[1].style.fg, Some(Color::DarkGray));
        assert_eq!(line.spans[3].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn format_log_entry_marks_semantic_errors_red() {
        let line = format_log_entry("[11:45:19.541 0068] error[E0425]: cannot find value");
        assert_eq!(
            line.spans.last().and_then(|span| span.style.fg),
            Some(Color::Red)
        );
    }

    #[test]
    fn format_log_entry_marks_semantic_warnings_yellow() {
        let line = format_log_entry("[11:45:19.541 0068] warning: field is never read");
        assert_eq!(
            line.spans.last().and_then(|span| span.style.fg),
            Some(Color::Yellow)
        );
    }

    #[test]
    fn format_log_entry_marks_semantic_success_green() {
        let line = format_log_entry("[11:45:19.541 0068] test result: ok. 180 passed; 0 failed");
        assert_eq!(
            line.spans.last().and_then(|span| span.style.fg),
            Some(Color::Green)
        );
    }

    #[test]
    fn format_log_entry_marks_semantic_skips_yellow() {
        let line = format_log_entry("[11:45:19.541 0068] 3 skipped, 2 ignored");
        assert_eq!(
            line.spans.last().and_then(|span| span.style.fg),
            Some(Color::Yellow)
        );
    }

    #[test]
    fn format_log_entry_prefers_test_outcome_over_error_in_test_name() {
        let line = format_log_entry(
            "[11:45:19.541 0068] test pipeline::rules::tests::captures_error_for_ambiguous_git_tag_context ... ok",
        );
        assert_eq!(
            line.spans.last().and_then(|span| span.style.fg),
            Some(Color::Green)
        );
    }

    #[test]
    fn format_log_entry_marks_failed_test_outcomes_red() {
        let line = format_log_entry(
            "[11:45:19.541 0068] test env::tests::expands_env_references ... FAILED",
        );
        assert_eq!(
            line.spans.last().and_then(|span| span.style.fg),
            Some(Color::Red)
        );
    }

    #[test]
    fn current_log_path_ignores_missing_job_logs() {
        let temp = tempdir().expect("tempdir");
        let state = UiState::new(
            vec![UiJobInfo {
                name: "lint".to_string(),
                source_name: "lint".to_string(),
                stage: "test".to_string(),
                log_path: temp.path().join("missing.log"),
                log_hash: "abc123".to_string(),
                runner: UiRunnerInfo::default(),
            }],
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        assert!(state.current_log_path().is_none());
    }

    #[test]
    fn current_log_path_returns_existing_job_log() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("job.log");
        std::fs::write(&log_path, "hello").expect("write log");
        let state = UiState::new(
            vec![UiJobInfo {
                name: "lint".to_string(),
                source_name: "lint".to_string(),
                stage: "test".to_string(),
                log_path: log_path.clone(),
                log_hash: "abc123".to_string(),
                runner: UiRunnerInfo::default(),
            }],
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        assert_eq!(state.current_log_path(), Some(log_path));
    }

    #[test]
    fn view_history_run_loads_first_history_job_preview() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("history-job.log");
        std::fs::write(&log_path, "from history").expect("write log");
        let history = vec![HistoryEntry {
            run_id: "run-2".to_string(),
            finished_at: "2026-03-27T00:00:00Z".to_string(),
            status: HistoryStatus::Success,
            jobs: vec![HistoryJob {
                name: "unit-tests".to_string(),
                stage: "test".to_string(),
                status: HistoryStatus::Success,
                log_hash: "hash123".to_string(),
                log_path: Some(log_path.display().to_string()),
                artifact_dir: None,
                artifacts: Vec::new(),
                caches: Vec::new(),
                container_name: None,
                service_network: None,
                service_containers: Vec::new(),
                runtime_summary_path: None,
            }],
        }];
        let mut state = UiState::new(
            Vec::new(),
            history,
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        state.view_history_run("run-2").expect("view history run");

        let view = state.history_view.as_ref().expect("history view");
        assert_eq!(view.selected, 0);
        assert_eq!(view.jobs[0].name, "unit-tests");
        let preview = state.history_preview.as_ref().expect("history preview");
        assert!(preview.title.contains("run-2 • unit-tests"));
        assert!(preview.lines.iter().any(|line| line == "from history"));
    }

    #[test]
    fn view_history_job_selects_matching_job() {
        let temp = tempdir().expect("tempdir");
        let first_log = temp.path().join("first.log");
        let second_log = temp.path().join("second.log");
        std::fs::write(&first_log, "first").expect("write first log");
        std::fs::write(&second_log, "second").expect("write second log");
        let history = vec![HistoryEntry {
            run_id: "run-2".to_string(),
            finished_at: "2026-03-27T00:00:00Z".to_string(),
            status: HistoryStatus::Success,
            jobs: vec![
                HistoryJob {
                    name: "lint".to_string(),
                    stage: "test".to_string(),
                    status: HistoryStatus::Success,
                    log_hash: "hash1".to_string(),
                    log_path: Some(first_log.display().to_string()),
                    artifact_dir: None,
                    artifacts: Vec::new(),
                    caches: Vec::new(),
                    container_name: None,
                    service_network: None,
                    service_containers: Vec::new(),
                    runtime_summary_path: None,
                },
                HistoryJob {
                    name: "unit-tests".to_string(),
                    stage: "test".to_string(),
                    status: HistoryStatus::Success,
                    log_hash: "hash2".to_string(),
                    log_path: Some(second_log.display().to_string()),
                    artifact_dir: None,
                    artifacts: Vec::new(),
                    caches: Vec::new(),
                    container_name: None,
                    service_network: None,
                    service_containers: Vec::new(),
                    runtime_summary_path: None,
                },
            ],
        }];
        let mut state = UiState::new(
            Vec::new(),
            history,
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        state
            .view_history_job("run-2", "unit-tests")
            .expect("view history job");

        let view = state.history_view.as_ref().expect("history view");
        assert_eq!(view.selected, 1);
        let preview = state.history_preview.as_ref().expect("history preview");
        assert!(preview.title.contains("run-2 • unit-tests"));
        assert!(preview.lines.iter().any(|line| line == "second"));
    }

    #[test]
    fn view_mode_can_open_most_recent_history_run_even_when_ids_match() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("latest.log");
        std::fs::write(&log_path, "latest history").expect("write latest log");
        let history = vec![HistoryEntry {
            run_id: "run-latest".to_string(),
            finished_at: "2026-03-27T00:00:00Z".to_string(),
            status: HistoryStatus::Success,
            jobs: vec![HistoryJob {
                name: "lint".to_string(),
                stage: "test".to_string(),
                status: HistoryStatus::Success,
                log_hash: "hash123".to_string(),
                log_path: Some(log_path.display().to_string()),
                artifact_dir: None,
                artifacts: Vec::new(),
                caches: Vec::new(),
                container_name: None,
                service_network: None,
                service_containers: Vec::new(),
                runtime_summary_path: None,
            }],
        }];

        let mut state = UiState::new(
            Vec::new(),
            history,
            "run-latest".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        state
            .view_history_run("run-latest")
            .expect("open latest history run");

        assert!(state.history_view.is_some());
        let preview = state.history_preview.as_ref().expect("history preview");
        assert!(preview.lines.iter().any(|line| line == "latest history"));
    }

    #[test]
    fn tab_density_compact_and_full_render_differently() {
        let temp = tempdir().expect("tempdir");
        let state = UiState::new(
            vec![UiJobInfo {
                name: "package-crate".to_string(),
                source_name: "package-crate".to_string(),
                stage: "package".to_string(),
                log_path: temp.path().join("job.log"),
                log_hash: "abc123".to_string(),
                runner: UiRunnerInfo::default(),
            }],
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        let job = state.active_jobs().first().expect("job");
        let compact = state
            .build_label_spans(job, false, true)
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let full = state
            .build_label_spans(job, false, false)
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_ne!(compact, full);
        assert!(!compact.contains(" · package"));
        assert!(full.contains(" · package"));
        assert!(full.contains("pending"));
    }

    #[test]
    fn help_navigation_cycles_back_to_shortcuts() {
        let temp = tempdir().expect("tempdir");
        let mut state = UiState::new(
            Vec::new(),
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        state.toggle_help();
        state.open_help_document(0);
        state.previous_help_document();
        assert!(matches!(state.help_view, HelpView::Shortcuts));

        state.open_help_document(0);
        while !matches!(state.help_view, HelpView::Shortcuts) {
            state.next_help_document();
        }
        assert!(matches!(state.help_view, HelpView::Shortcuts));
    }

    #[test]
    fn pane_focus_skips_hidden_panes() {
        let temp = tempdir().expect("tempdir");
        let mut state = UiState::new(
            vec![UiJobInfo {
                name: "lint: [linux]".to_string(),
                source_name: "lint".to_string(),
                stage: "test".to_string(),
                log_path: temp.path().join("job.log"),
                log_hash: "abc123".to_string(),
                runner: UiRunnerInfo::default(),
            }],
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        state.toggle_history_pane();
        state.toggle_job_yaml_pane();
        state.toggle_focus();
        assert!(state.focus_is_job_yaml());

        state.toggle_job_yaml_pane();
        assert!(matches!(state.focus, crate::ui::types::PaneFocus::Jobs));
        state.toggle_focus();
        assert!(matches!(state.focus, crate::ui::types::PaneFocus::History));
    }

    #[test]
    fn history_pane_is_hidden_by_default() {
        let temp = tempdir().expect("tempdir");
        let state = UiState::new(
            Vec::new(),
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        assert!(!state.history_pane_visible());
        assert!(matches!(state.focus, crate::ui::types::PaneFocus::Jobs));
    }

    #[test]
    fn job_yaml_panel_uses_source_job_name_lookup() {
        let temp = tempdir().expect("tempdir");
        let pipeline_path = temp.path().join(".gitlab-ci.yml");
        std::fs::write(
            &pipeline_path,
            "stages: [test]\nlint:\n  stage: test\n  script:\n    - cargo fmt --check\n",
        )
        .expect("write pipeline");

        let state = UiState::new(
            vec![UiJobInfo {
                name: "lint: [linux]".to_string(),
                source_name: "lint".to_string(),
                stage: "test".to_string(),
                log_path: temp.path().join("job.log"),
                log_hash: "abc123".to_string(),
                runner: UiRunnerInfo::default(),
            }],
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            pipeline_path,
        );

        let yaml = state.job_yaml_text_for_pager();
        assert!(yaml.contains("lint:"));
        assert!(yaml.contains("cargo fmt --check"));
    }

    #[test]
    fn details_panel_shows_runner_info() {
        let temp = tempdir().expect("tempdir");
        let state = UiState::new(
            vec![UiJobInfo {
                name: "build".to_string(),
                source_name: "build".to_string(),
                stage: "build".to_string(),
                log_path: temp.path().join("job.log"),
                log_hash: "abc123".to_string(),
                runner: UiRunnerInfo {
                    engine: "container".to_string(),
                    arch: Some("arm64".to_string()),
                    cpus: Some("6".to_string()),
                    memory: Some("3g".to_string()),
                },
            }],
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        let _ = state.info_panel();
        let job = state.active_job().expect("active job");
        assert_eq!(job.runner.engine, "container");
        assert_eq!(job.runner.arch.as_deref(), Some("arm64"));
        assert_eq!(job.runner.cpus.as_deref(), Some("6"));
        assert_eq!(job.runner.memory.as_deref(), Some("3g"));
    }

    #[test]
    fn tab_label_shows_ai_indicator_when_analysis_is_running() {
        let temp = tempdir().expect("tempdir");
        let mut state = UiState::new(
            vec![UiJobInfo {
                name: "build".to_string(),
                source_name: "build".to_string(),
                stage: "build".to_string(),
                log_path: temp.path().join("job.log"),
                log_hash: "abc123".to_string(),
                runner: UiRunnerInfo::default(),
            }],
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        state.analysis_started("build", "ollama");
        let job = state.active_jobs().first().expect("job");
        let rendered = state
            .build_label_spans(job, false, false)
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(rendered.contains("ai…"));
    }

    #[test]
    fn ai_prompt_preview_toggle_closes_existing_prompt_preview() {
        let temp = tempdir().expect("tempdir");
        let mut state = UiState::new(
            Vec::new(),
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        state.load_text_preview("AI Prompt • build".to_string(), "hello".to_string());
        assert!(state.toggle_ai_prompt_preview());
        assert!(state.history_preview.is_none());
    }

    #[test]
    fn analysis_finished_loads_final_text_into_analysis_view() {
        let temp = tempdir().expect("tempdir");
        let mut state = UiState::new(
            vec![UiJobInfo {
                name: "build".to_string(),
                source_name: "build".to_string(),
                stage: "build".to_string(),
                log_path: temp.path().join("job.log"),
                log_hash: "abc123".to_string(),
                runner: UiRunnerInfo::default(),
            }],
            Vec::new(),
            "run-1".to_string(),
            HashMap::new(),
            String::new(),
            temp.path().to_path_buf(),
            temp.path().join(".gitlab-ci.yml"),
        );

        state.analysis_started("build", "codex");
        state.analysis_finished(
            "build",
            "Root cause\n\nSomething broke".to_string(),
            None,
            None,
        );

        let text = state.current_analysis_text().expect("analysis text");
        assert!(text.contains("provider: codex"));
        assert!(text.contains("Root cause"));
        assert!(text.contains("Something broke"));
    }
}
