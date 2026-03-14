use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Stdout, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyCode, KeyEvent,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use owo_colors::OwoColorize;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::history::{HistoryEntry, HistoryJob, HistoryStatus};

const LOG_SCROLL_STEP: usize = 3;
const LOG_SCROLL_HALF: usize = 20;
const LOG_SCROLL_PAGE: usize = 60;
const CURRENT_HISTORY_KEY: &str = "__current_run__";

#[derive(Clone, Copy, PartialEq, Eq)]
enum PaneFocus {
    History,
    Jobs,
}

#[derive(Clone)]
enum HistoryAction {
    SelectJob(usize),
    ViewLog { title: String, path: PathBuf },
    ViewRun(String),
}

#[derive(Clone)]
pub struct UiJobInfo {
    pub name: String,
    pub stage: String,
    pub log_path: PathBuf,
    pub log_hash: String,
}

pub struct UiHandle {
    sender: UnboundedSender<UiEvent>,
    command_rx: Mutex<Option<UnboundedReceiver<UiCommand>>>,
    thread: thread::JoinHandle<()>,
}

#[derive(Clone)]
pub struct UiBridge {
    sender: UnboundedSender<UiEvent>,
}

#[derive(Clone)]
pub enum UiCommand {
    RestartJob { name: String },
}

impl UiHandle {
    pub fn start(
        jobs: Vec<UiJobInfo>,
        history: Vec<HistoryEntry>,
        current_run_id: String,
    ) -> Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let thread_tx = tx.clone();
        let handle = thread::spawn(move || {
            if let Err(err) = UiRunner::new(jobs, history, current_run_id, rx, cmd_tx).run() {
                eprintln!("ui error: {err:?}");
            }
        });
        Ok(Self {
            sender: thread_tx,
            command_rx: Mutex::new(Some(cmd_rx)),
            thread: handle,
        })
    }

    pub fn bridge(&self) -> UiBridge {
        UiBridge {
            sender: self.sender.clone(),
        }
    }

    pub fn command_receiver(&self) -> Option<UnboundedReceiver<UiCommand>> {
        self.command_rx
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
    }

    pub fn pipeline_finished(&self) {
        let _ = self.sender.send(UiEvent::PipelineFinished);
    }

    pub fn wait_for_exit(self) {
        let _ = self.thread.join();
    }
}

impl UiBridge {
    pub fn job_started(&self, name: &str) {
        let _ = self.sender.send(UiEvent::JobStarted {
            name: name.to_string(),
        });
    }

    pub fn job_restarted(&self, name: &str) {
        let _ = self.sender.send(UiEvent::JobRestarted {
            name: name.to_string(),
        });
    }

    pub fn history_updated(&self, entry: HistoryEntry) {
        let _ = self.sender.send(UiEvent::HistoryUpdated { entry });
    }

    pub fn job_log_line(&self, name: &str, line: &str) {
        let _ = self.sender.send(UiEvent::JobLog {
            name: name.to_string(),
            line: line.to_string(),
        });
    }

    pub fn job_finished(
        &self,
        name: &str,
        status: UiJobStatus,
        duration: f32,
        error: Option<String>,
    ) {
        let _ = self.sender.send(UiEvent::JobFinished {
            name: name.to_string(),
            status,
            duration,
            error,
        });
    }
}

enum UiEvent {
    JobStarted {
        name: String,
    },
    JobRestarted {
        name: String,
    },
    JobLog {
        name: String,
        line: String,
    },
    JobFinished {
        name: String,
        status: UiJobStatus,
        duration: f32,
        error: Option<String>,
    },
    HistoryUpdated {
        entry: HistoryEntry,
    },
    PipelineFinished,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UiJobStatus {
    Pending,
    Running,
    Success,
    Failed,
    Skipped,
}

struct UiRunner {
    rx: UnboundedReceiver<UiEvent>,
    commands: UnboundedSender<UiCommand>,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    state: UiState,
    pipeline_finished: bool,
    exit_requested: bool,
}

impl UiRunner {
    fn new(
        jobs: Vec<UiJobInfo>,
        history: Vec<HistoryEntry>,
        current_run_id: String,
        rx: UnboundedReceiver<UiEvent>,
        commands: UnboundedSender<UiCommand>,
    ) -> Self {
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).expect("failed to create terminal");
        Self {
            rx,
            commands,
            terminal,
            state: UiState::new(jobs, history, current_run_id),
            pipeline_finished: false,
            exit_requested: false,
        }
    }

    fn run(mut self) -> Result<()> {
        enable_raw_mode().context("failed to enable raw mode")?;
        io::stdout()
            .execute(EnterAlternateScreen)
            .context("failed to enter alternate screen")?;
        io::stdout()
            .execute(EnableMouseCapture)
            .context("failed to enable mouse capture")?;

        let result = (|| -> Result<()> {
            while !self.should_quit() {
                self.draw()?;
                self.drain_events();
                self.handle_input()?;
            }
            Ok(())
        })();

        disable_raw_mode().context("failed to disable raw mode")?;
        io::stdout()
            .execute(DisableMouseCapture)
            .context("failed to disable mouse capture")?;
        io::stdout()
            .execute(LeaveAlternateScreen)
            .context("failed to leave alternate screen")?;
        result
    }

    fn should_quit(&self) -> bool {
        self.exit_requested
    }

    fn draw(&mut self) -> Result<()> {
        self.terminal.draw(|frame| {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(32), Constraint::Min(0)])
                .split(frame.size());

            let history = self.state.history_widget(columns[0].height);
            frame.render_widget(history, columns[0]);

            let tab_width = columns[1].width.saturating_sub(2).max(1);
            let (tabs, tab_height) = self.state.tabs(tab_width);

            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(tab_height),
                    Constraint::Length(4),
                    Constraint::Min(0),
                    Constraint::Length(2),
                ])
                .split(columns[1]);

            frame.render_widget(tabs, layout[0]);

            let info = self.state.info_panel();
            frame.render_widget(info, layout[1]);

            let log_widget =
                self.state
                    .log_view(self.pipeline_finished, layout[2].width, layout[2].height);
            frame.render_widget(log_widget, layout[2]);

            let hint = self.state.key_hint_widget();
            frame.render_widget(hint, layout[3]);
        })?;
        Ok(())
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                UiEvent::JobStarted { name } => self.state.set_status(&name, UiJobStatus::Running),
                UiEvent::JobRestarted { name } => self.state.restart_job(&name),
                UiEvent::JobLog { name, line } => self.state.push_log(&name, line),
                UiEvent::JobFinished {
                    name,
                    status,
                    duration,
                    error,
                } => self.state.finish_job(&name, status, duration, error),
                UiEvent::HistoryUpdated { entry } => self.state.push_history_entry(entry),
                UiEvent::PipelineFinished => self.pipeline_finished = true,
            }
        }
    }

    fn handle_input(&mut self) -> Result<()> {
        if !event::poll(Duration::from_millis(50))? {
            return Ok(());
        }

        match event::read()? {
            CEvent::Key(key) => self.handle_key(key),
            CEvent::Mouse(mouse) => {
                self.handle_mouse(mouse);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        let modifiers = key.modifiers;
        match key.code {
            KeyCode::Char('q') => {
                self.exit_requested = true;
                return Ok(());
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.exit_requested = true;
                return Ok(());
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.state.toggle_focus();
                return Ok(());
            }
            _ => {}
        }

        if self.state.focus_is_history() {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
                    self.state.history_move_down()
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
                    self.state.history_move_up()
                }
                KeyCode::Left | KeyCode::Char('h') => self.state.history_move_left(),
                KeyCode::Right | KeyCode::Char('l') => self.state.history_move_right(),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(action) = self.state.history_activate() {
                        match action {
                            HistoryAction::SelectJob(idx) => self.state.select_job(idx),
                            HistoryAction::ViewLog { title, path } => {
                                if let Err(err) =
                                    self.state.load_history_preview(title.clone(), &path)
                                {
                                    self.state.set_history_preview_message(
                                        title,
                                        &path,
                                        format!("failed to load log: {err}"),
                                    );
                                }
                            }
                            HistoryAction::ViewRun(run_id) => {
                                if let Err(err) = self.state.view_history_run(&run_id) {
                                    let title = format!("{run_id} • history");
                                    let empty = PathBuf::new();
                                    self.state.set_history_preview_message(
                                        title,
                                        &empty,
                                        err.to_string(),
                                    );
                                }
                            }
                        }
                    }
                }
                KeyCode::Home => self.state.history_move_home(),
                KeyCode::End => self.state.history_move_end(),
                _ => {}
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Char('J') => self.state.next_job(),
            KeyCode::Char('k') | KeyCode::Char('K') => self.state.previous_job(),
            KeyCode::Char('h') => self.state.previous_job(),
            KeyCode::Char('l') => self.state.next_job(),
            KeyCode::Left => self.state.previous_job(),
            KeyCode::Right => self.state.next_job(),
            KeyCode::Down => {
                if modifiers.contains(KeyModifiers::SHIFT)
                    || modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.state.scroll_logs_line_down();
                } else {
                    self.state.next_job();
                }
            }
            KeyCode::Up => {
                if modifiers.contains(KeyModifiers::SHIFT)
                    || modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.state.scroll_logs_line_up();
                } else {
                    self.state.previous_job();
                }
            }
            KeyCode::PageDown => self.state.scroll_logs_page_down(),
            KeyCode::PageUp => self.state.scroll_logs_page_up(),
            KeyCode::End => self.state.scroll_bottom(),
            KeyCode::Home => self.state.scroll_top(),
            KeyCode::Char('g') if !modifiers.contains(KeyModifiers::SHIFT) => {
                self.state.scroll_top();
            }
            KeyCode::Char('G') => self.state.scroll_bottom(),
            KeyCode::Char(' ') => self.state.scroll_logs_page_down(),
            KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.scroll_logs_half_down()
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.scroll_logs_half_up()
            }
            KeyCode::Char('f') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.scroll_logs_page_down()
            }
            KeyCode::Char('b') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.scroll_logs_page_up()
            }
            KeyCode::Char('e') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.scroll_logs_line_down()
            }
            KeyCode::Char('y') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.scroll_logs_line_up()
            }
            KeyCode::Char('r') => {
                if let Some(name) = self.state.restartable_job_name() {
                    let _ = self.commands.send(UiCommand::RestartJob { name });
                }
            }
            KeyCode::Char('o') => {
                self.view_current_log()?;
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_mouse(&mut self, event: MouseEvent) {
        match event.kind {
            MouseEventKind::ScrollUp => {
                if self.state.focus_is_history() {
                    self.state.history_move_up();
                } else {
                    self.state.scroll_logs_mouse_up();
                }
            }
            MouseEventKind::ScrollDown => {
                if self.state.focus_is_history() {
                    self.state.history_move_down();
                } else {
                    self.state.scroll_logs_mouse_down();
                }
            }
            _ => {}
        }
    }

    fn view_current_log(&mut self) -> Result<()> {
        if let Some(path) = self.state.current_log_path() {
            self.suspend_terminal(|| page_log_with_colors(&path))
        } else {
            Ok(())
        }
    }

    fn suspend_terminal<F>(&mut self, action: F) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        disable_raw_mode().ok();
        let _ = io::stdout().execute(DisableMouseCapture);
        let _ = io::stdout().execute(LeaveAlternateScreen);
        let result = action();
        let _ = io::stdout().execute(EnterAlternateScreen);
        let _ = io::stdout().execute(EnableMouseCapture);
        enable_raw_mode().ok();
        self.terminal.clear()?;
        result
    }
}

struct UiState {
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
}

impl UiState {
    fn new(jobs: Vec<UiJobInfo>, history: Vec<HistoryEntry>, current_run_id: String) -> Self {
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
        }
    }

    fn active_jobs(&self) -> &[UiJobState] {
        if let Some(view) = &self.history_view {
            &view.jobs
        } else {
            &self.jobs
        }
    }

    fn active_selected_index(&self) -> usize {
        if let Some(view) = &self.history_view {
            view.selected
        } else {
            self.selected
        }
    }

    fn set_active_selected_index(&mut self, idx: usize) {
        if let Some(view) = &mut self.history_view {
            if idx < view.jobs.len() {
                view.selected = idx;
            }
        } else if idx < self.jobs.len() {
            self.selected = idx;
        }
        self.on_active_selection_changed();
    }

    fn active_job(&self) -> Option<&UiJobState> {
        self.active_jobs().get(self.active_selected_index())
    }

    fn active_job_mut(&mut self) -> Option<&mut UiJobState> {
        let idx = self.active_selected_index();
        if let Some(view) = &mut self.history_view {
            view.jobs.get_mut(idx)
        } else {
            self.jobs.get_mut(idx)
        }
    }

    fn tabs(&self, width: u16) -> (Paragraph<'static>, u16) {
        let (lines, rows) = self.tab_lines(width as usize);
        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Jobs"))
            .wrap(Wrap { trim: false });

        let content_height = rows.saturating_add(2); // account for top/bottom borders

        (paragraph, content_height)
    }

    fn history_widget(&mut self, height: u16) -> Paragraph<'static> {
        let nodes = self.history_nodes();
        if nodes.is_empty() {
            self.history_selection = 0;
            self.history_scroll = 0;
            self.clear_history_preview();
            return Paragraph::new(vec![Line::from("no runs recorded")])
                .block(Block::default().borders(Borders::ALL).title("Runs"))
                .wrap(Wrap { trim: false });
        }

        self.clamp_history_selection(nodes.len());
        self.ensure_history_visible(height, nodes.len());

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

    fn history_status_style(status: HistoryStatus) -> Style {
        match status {
            HistoryStatus::Success => Style::default().fg(Color::Green),
            HistoryStatus::Failed => Style::default().fg(Color::Red),
            HistoryStatus::Skipped => Style::default().fg(Color::Yellow),
            HistoryStatus::Running => Style::default().fg(Color::Cyan),
        }
    }

    fn history_preview_view(&self, width: u16, height: u16) -> Paragraph<'static> {
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

    fn scroll_history_preview_up(&mut self, lines: usize) {
        if let Some(preview) = self.history_preview.as_mut() {
            preview.scroll_lines_up(lines);
        }
    }

    fn scroll_history_preview_down(&mut self, lines: usize) {
        if let Some(preview) = self.history_preview.as_mut() {
            preview.scroll_lines_down(lines);
        }
    }

    fn scroll_history_preview_to_top(&mut self) {
        if let Some(preview) = self.history_preview.as_mut() {
            preview.scroll_to_top();
        }
    }

    fn scroll_history_preview_to_bottom(&mut self) {
        if let Some(preview) = self.history_preview.as_mut() {
            preview.scroll_to_bottom();
        }
    }

    fn history_nodes(&self) -> Vec<HistoryRenderNode> {
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
            }
        }

        nodes
    }

    fn history_job_line(
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

    fn history_status_from_ui(status: UiJobStatus) -> HistoryStatus {
        match status {
            UiJobStatus::Success => HistoryStatus::Success,
            UiJobStatus::Failed => HistoryStatus::Failed,
            UiJobStatus::Skipped => HistoryStatus::Skipped,
            UiJobStatus::Running | UiJobStatus::Pending => HistoryStatus::Running,
        }
    }

    fn current_run_status(&self) -> HistoryStatus {
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

    fn apply_history_highlight(mut line: Line<'static>) -> Line<'static> {
        let highlight = Style::default()
            .bg(Color::DarkGray)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        for span in &mut line.spans {
            span.style = span.style.patch(highlight);
        }
        line
    }

    fn is_run_collapsed(&self, key: &str) -> bool {
        if key == CURRENT_HISTORY_KEY {
            self.history_collapsed.get(key).copied().unwrap_or(false)
        } else {
            self.history_collapsed.get(key).copied().unwrap_or(true)
        }
    }

    fn set_run_collapsed(&mut self, key: &str, collapsed: bool) {
        self.history_collapsed.insert(key.to_string(), collapsed);
    }

    fn toggle_focus(&mut self) {
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

    fn focus_is_history(&self) -> bool {
        matches!(self.focus, PaneFocus::History)
    }

    fn push_history_entry(&mut self, entry: HistoryEntry) {
        self.history_collapsed
            .entry(entry.run_id.clone())
            .or_insert(true);
        self.history.push(entry);
    }

    fn view_history_run(&mut self, run_id: &str) -> Result<()> {
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

    fn close_history_view(&mut self) {
        self.history_view = None;
        self.history_preview = None;
        self.focus = PaneFocus::Jobs;
        if self.selected >= self.jobs.len() && !self.jobs.is_empty() {
            self.selected = self.jobs.len() - 1;
        }
        self.on_active_selection_changed();
    }

    fn history_move_up(&mut self) {
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
    }

    fn history_move_down(&mut self) {
        let nodes = self.history_nodes();
        if nodes.is_empty() {
            self.history_selection = 0;
            return;
        }
        self.clear_history_preview();
        if self.history_selection + 1 < nodes.len() {
            self.history_selection += 1;
        }
    }

    fn history_move_home(&mut self) {
        self.clear_history_preview();
        self.history_selection = 0;
        self.history_scroll = 0;
    }

    fn history_move_end(&mut self) {
        let len = self.history_nodes().len();
        if len == 0 {
            self.history_selection = 0;
            self.history_scroll = 0;
        } else {
            self.history_selection = len - 1;
        }
        self.clear_history_preview();
    }

    fn history_move_left(&mut self) {
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
    }

    fn history_move_right(&mut self) {
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
    }

    fn history_activate(&mut self) -> Option<HistoryAction> {
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
        }
    }

    fn clear_history_preview(&mut self) {
        self.history_preview = None;
    }

    fn load_history_preview(&mut self, title: String, path: &Path) -> Result<()> {
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

    fn set_history_preview_message(&mut self, title: String, path: &Path, message: String) {
        self.history_preview = Some(HistoryPreview {
            title,
            path: path.to_path_buf(),
            lines: vec![message],
            scroll_offset: 0,
        });
        self.focus = PaneFocus::Jobs;
    }

    fn on_active_selection_changed(&mut self) {
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

    fn clamp_history_selection(&mut self, len: usize) {
        if len == 0 {
            self.history_selection = 0;
        } else if self.history_selection >= len {
            self.history_selection = len - 1;
        }
    }

    fn ensure_history_visible(&mut self, height: u16, len: usize) {
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

    fn history_viewport(height: u16) -> usize {
        usize::from(height.saturating_sub(2).max(1))
    }

    fn tab_lines(&self, available: usize) -> (Vec<Line<'static>>, u16) {
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

    fn build_label_spans(&self, job: &UiJobState, selected: bool) -> Vec<Span<'static>> {
        let (icon_char, icon_color) = job.status.icon();
        let highlight = if selected {
            Some(
                Style::default()
                    .bg(Color::Black)
                    .fg(Color::White)
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

    fn apply_highlight(base: Style, highlight: Option<Style>) -> Style {
        if let Some(highlight_style) = highlight {
            base.patch(highlight_style)
        } else {
            base
        }
    }

    fn key_hint_widget(&self) -> Paragraph<'static> {
        Paragraph::new(self.key_hint_line())
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false })
    }

    fn key_hint_line(&self) -> Line<'static> {
        Line::from(vec![
            Span::styled("Keys: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                "Tab switches pane • History: ↑/↓ move, ←/→ collapse, Enter views run/log • Jobs: j/k/h/l arrows switch tabs • Shift/Ctrl+↑/↓ PgUp/PgDn Ctrl+u/d/f/b g/G wheel scroll logs • o opens log • r restarts job • q exits",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    }

    fn info_panel(&self) -> Paragraph<'_> {
        let job = match self.active_job() {
            Some(job) => job,
            None => {
                return Paragraph::new(vec![Line::from("No job selected")])
                    .block(Block::default().borders(Borders::ALL).title("Details"))
                    .wrap(Wrap { trim: true });
            }
        };
        let lines = vec![
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
                Span::raw(format!("{} ({:.2}s)", job.status.label(), job.duration)),
            ]),
        ];

        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Details"))
            .wrap(Wrap { trim: true })
    }

    fn log_view(&self, pipeline_finished: bool, width: u16, height: u16) -> Paragraph<'_> {
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
            format!("Status: {} ({:.2}s)", job.status.label(), job.duration),
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

    fn current_log_path(&self) -> Option<PathBuf> {
        self.active_job().map(|job| job.log_path.clone())
    }

    fn restartable_job_name(&self) -> Option<String> {
        if self.history_view.is_some() {
            return None;
        }
        self.jobs
            .get(self.selected)
            .and_then(|job| job.status.is_restartable().then(|| job.name.clone()))
    }

    fn restart_job(&mut self, name: &str) {
        if let Some(idx) = self.order.get(name).copied() {
            self.jobs[idx].reset_for_restart();
        }
    }

    fn set_status(&mut self, name: &str, status: UiJobStatus) {
        if let Some(idx) = self.order.get(name).copied() {
            self.jobs[idx].status = status;
        }
    }

    fn push_log(&mut self, name: &str, line: String) {
        if let Some(idx) = self.order.get(name).copied() {
            self.jobs[idx].push_log(line);
        }
    }

    fn finish_job(
        &mut self,
        name: &str,
        status: UiJobStatus,
        duration: f32,
        error: Option<String>,
    ) {
        if let Some(idx) = self.order.get(name).copied() {
            self.jobs[idx].status = status;
            self.jobs[idx].duration = duration;
            self.jobs[idx].error = error;
        }
    }

    fn next_job(&mut self) {
        let len = self.active_jobs().len();
        if len == 0 {
            return;
        }
        self.clear_history_preview();
        let next = (self.active_selected_index() + 1) % len;
        self.set_active_selected_index(next);
    }

    fn previous_job(&mut self) {
        let len = self.active_jobs().len();
        if len == 0 {
            return;
        }
        self.clear_history_preview();
        let current = self.active_selected_index();
        let prev = if current == 0 { len - 1 } else { current - 1 };
        self.set_active_selected_index(prev);
    }

    fn select_job(&mut self, idx: usize) {
        let len = self.active_jobs().len();
        if idx >= len {
            return;
        }
        self.clear_history_preview();
        self.focus = PaneFocus::Jobs;
        self.set_active_selected_index(idx);
    }

    fn scroll_logs_line_up(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_up(LOG_SCROLL_STEP);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_up(LOG_SCROLL_STEP);
        }
    }

    fn scroll_logs_line_down(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_down(LOG_SCROLL_STEP);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_down(LOG_SCROLL_STEP);
        }
    }

    fn scroll_logs_half_up(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_up(LOG_SCROLL_HALF);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_up(LOG_SCROLL_HALF);
        }
    }

    fn scroll_logs_half_down(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_down(LOG_SCROLL_HALF);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_down(LOG_SCROLL_HALF);
        }
    }

    fn scroll_logs_page_up(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_up(LOG_SCROLL_PAGE);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_up(LOG_SCROLL_PAGE);
        }
    }

    fn scroll_logs_page_down(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_down(LOG_SCROLL_PAGE);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_down(LOG_SCROLL_PAGE);
        }
    }

    fn scroll_logs_mouse_up(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_up(LOG_SCROLL_STEP);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_up(LOG_SCROLL_STEP);
        }
    }

    fn scroll_logs_mouse_down(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_down(LOG_SCROLL_STEP);
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_lines_down(LOG_SCROLL_STEP);
        }
    }

    fn scroll_top(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_to_top();
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_to_top();
        }
    }

    fn scroll_bottom(&mut self) {
        if self.history_preview.is_some() {
            self.scroll_history_preview_to_bottom();
            return;
        }
        if let Some(job) = self.active_job_mut() {
            job.scroll_to_bottom();
        }
    }
}

struct UiJobState {
    name: String,
    stage: String,
    log_path: PathBuf,
    log_hash: String,
    status: UiJobStatus,
    duration: f32,
    error: Option<String>,
    logs: Vec<String>,
    scroll_offset: usize,
    follow_logs: bool,
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
            error: None,
            logs: Vec::new(),
            scroll_offset: 0,
            follow_logs: true,
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
            error: None,
            logs: Vec::new(),
            scroll_offset: 0,
            follow_logs: true,
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
}

enum ScrollDirection {
    Up,
    Down,
}

struct HistoryRenderNode {
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

    fn is_restartable(self) -> bool {
        matches!(self, UiJobStatus::Success | UiJobStatus::Failed)
    }

    fn label(self) -> &'static str {
        match self {
            UiJobStatus::Pending => "pending",
            UiJobStatus::Running => "running",
            UiJobStatus::Success => "success",
            UiJobStatus::Failed => "failed",
            UiJobStatus::Skipped => "skipped",
        }
    }

    fn from_history(status: HistoryStatus) -> Self {
        match status {
            HistoryStatus::Success => UiJobStatus::Success,
            HistoryStatus::Failed => UiJobStatus::Failed,
            HistoryStatus::Skipped => UiJobStatus::Skipped,
            HistoryStatus::Running => UiJobStatus::Running,
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

fn page_log_with_colors(path: &Path) -> Result<()> {
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
