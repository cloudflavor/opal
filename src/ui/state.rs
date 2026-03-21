use super::types::{
    CURRENT_HISTORY_KEY, HistoryAction, LOG_SCROLL_HALF, LOG_SCROLL_PAGE, LOG_SCROLL_STEP,
    PaneFocus, UiJobInfo, UiJobResources, UiJobStatus,
};
use crate::history::{HistoryEntry, HistoryJob, HistoryStatus};
use crate::runtime;
use anyhow::{Context, Result, anyhow};
use include_dir::{Dir, include_dir};
use owo_colors::OwoColorize;
use ratatui::layout::Alignment;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, ScrollbarState, Wrap};
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
    plan_text: String,
    workdir: PathBuf,
    history_height: u16,
    loaded_dirs: HashSet<PathBuf>,
    has_current_run: bool,
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

        let mut collapsed_nodes = HashMap::new();
        collapsed_nodes.insert(Self::run_collapse_key(CURRENT_HISTORY_KEY), false);
        for entry in &history {
            collapsed_nodes.insert(Self::run_collapse_key(&entry.run_id), true);
        }

        let has_current_run = !job_states.is_empty();

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
            plan_text,
            workdir,
            history_height: 0,
            loaded_dirs: HashSet::new(),
            has_current_run,
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

    pub(super) fn history_widget(&mut self, height: u16) -> (List<'static>, ScrollbarState) {
        let nodes = self.history_nodes();
        if nodes.is_empty() {
            self.history_selection = 0;
            self.history_scroll = 0;
            self.clear_history_preview();
            let list = List::new(vec![ListItem::new(Line::from("no runs recorded"))])
                .block(Block::default().borders(Borders::ALL).title("Runs"));
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

        let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Runs"));
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
            let log_path = job
                .log_path
                .as_ref()
                .map(PathBuf::from)
                .or_else(|| Some(self.default_log_path(&entry.run_id, &job.log_hash)));
            let resources = UiJobResources::from(job);
            children.push(HistoryTreeEntry {
                key: HistoryNodeKey::FinishedJob {
                    run_id: entry.run_id.clone(),
                    job_name: job.name.clone(),
                    log_path,
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

    fn default_log_path(&self, run_id: &str, log_hash: &str) -> PathBuf {
        runtime::logs_dir(run_id).join(format!("{log_hash}.log"))
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
            .bg(Color::DarkGray)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        for span in &mut line.spans {
            span.style = span.style.patch(highlight);
        }
        line
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
        self.collapsed_nodes
            .entry(Self::run_collapse_key(&entry.run_id))
            .or_insert(true);
        self.history.push(entry);
        self.loaded_dirs.clear();
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
        if job.manual_pending {
            spans.push(Span::styled(
                " • MANUAL",
                Self::apply_highlight(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                    highlight,
                ),
            ));
        } else {
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

    fn from_history(run_id: &str, job: &HistoryJob, _workdir: &Path) -> Self {
        let log_path = job
            .log_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| runtime::logs_dir(run_id).join(format!("{}.log", job.log_hash)));
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
    FileEntry {
        path: PathBuf,
        is_dir: bool,
    },
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

#[cfg(test)]
mod tests {
    use super::{HelpDocument, is_markdown_path, render_markdown_for_pager};
    use ratatui::style::{Color, Modifier};
    use std::path::Path;

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
}
