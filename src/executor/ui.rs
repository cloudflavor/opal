use std::collections::{HashMap, VecDeque};
use std::env;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyCode, KeyEvent,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

const LOG_CAPACITY: usize = 1024;
const LOG_SCROLL_STEP: usize = 3;
const LOG_SCROLL_HALF: usize = 20;
const LOG_SCROLL_PAGE: usize = 60;

#[derive(Clone)]
pub struct UiJobInfo {
    pub name: String,
    pub stage: String,
    pub log_path: PathBuf,
    pub log_hash: String,
}

pub struct UiHandle {
    sender: UnboundedSender<UiEvent>,
    thread: thread::JoinHandle<()>,
}

#[derive(Clone)]
pub struct UiBridge {
    sender: UnboundedSender<UiEvent>,
}

impl UiHandle {
    pub fn start(jobs: Vec<UiJobInfo>) -> Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let thread_tx = tx.clone();
        let handle = thread::spawn(move || {
            if let Err(err) = UiRunner::new(jobs, rx).run() {
                eprintln!("ui error: {err:?}");
            }
        });
        Ok(Self {
            sender: thread_tx,
            thread: handle,
        })
    }

    pub fn bridge(&self) -> UiBridge {
        UiBridge {
            sender: self.sender.clone(),
        }
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
    terminal: Terminal<CrosstermBackend<Stdout>>,
    state: UiState,
    pipeline_finished: bool,
    exit_requested: bool,
}

impl UiRunner {
    fn new(jobs: Vec<UiJobInfo>, rx: UnboundedReceiver<UiEvent>) -> Self {
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).expect("failed to create terminal");
        Self {
            rx,
            terminal,
            state: UiState::new(jobs),
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
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(4),
                    Constraint::Min(0),
                    Constraint::Length(2),
                ])
                .split(frame.size());

            let tabs = self.state.tabs(layout[0].width.saturating_sub(2).max(1));
            frame.render_widget(tabs, layout[0]);

            let info = self.state.info_panel();
            frame.render_widget(info, layout[1]);

            let log_height = layout[2].height;
            let log_widget = self.state.log_view(self.pipeline_finished, log_height);
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
                UiEvent::JobLog { name, line } => self.state.push_log(&name, line),
                UiEvent::JobFinished {
                    name,
                    status,
                    duration,
                    error,
                } => self.state.finish_job(&name, status, duration, error),
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
            }
            KeyCode::Char('j') | KeyCode::Char('J') => self.state.next_job(),
            KeyCode::Char('k') | KeyCode::Char('K') => self.state.previous_job(),
            KeyCode::Char('h') => self.state.previous_job(),
            KeyCode::Char('l') => self.state.next_job(),
            KeyCode::Left => self.state.previous_job(),
            KeyCode::Right => self.state.next_job(),
            KeyCode::Tab => self.state.next_job(),
            KeyCode::BackTab => self.state.previous_job(),
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
            KeyCode::Char('o') => {
                self.view_current_log()?;
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.exit_requested = true;
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_mouse(&mut self, event: MouseEvent) {
        match event.kind {
            MouseEventKind::ScrollUp => self.state.scroll_logs_mouse_up(),
            MouseEventKind::ScrollDown => self.state.scroll_logs_mouse_down(),
            _ => {}
        }
    }

    fn view_current_log(&mut self) -> Result<()> {
        let path = self.state.current_log_path();
        self.suspend_terminal(|| {
            let pager = env::var("PAGER").unwrap_or_else(|_| "less".to_string());
            let status = Command::new(&pager).arg(&path).status();
            match status {
                Ok(status) if status.success() => Ok(()),
                Ok(_) | Err(_) => {
                    let _ = Command::new("cat").arg(&path).status();
                    Ok(())
                }
            }
        })
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
}

impl UiState {
    fn new(jobs: Vec<UiJobInfo>) -> Self {
        let mut order = HashMap::new();
        let job_states: Vec<UiJobState> = jobs
            .into_iter()
            .enumerate()
            .map(|(idx, job)| {
                order.insert(job.name.clone(), idx);
                UiJobState::from(job)
            })
            .collect();

        Self {
            jobs: job_states,
            order,
            selected: 0,
        }
    }

    fn tabs(&self, width: u16) -> Paragraph<'static> {
        let lines = self.tab_lines(width as usize);
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Jobs"))
            .wrap(Wrap { trim: false })
    }

    fn tab_lines(&self, available: usize) -> Vec<Line<'static>> {
        if self.jobs.is_empty() {
            return vec![Line::raw("")];
        }

        let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut width = 0usize;

        for (idx, job) in self.jobs.iter().enumerate() {
            let label_spans = self.build_label_spans(job, idx == self.selected);
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

        rows.into_iter().map(Line::from).collect()
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
                "j/k/h/l arrows switch tabs • Shift/Ctrl+↑/↓ PgUp/PgDn Ctrl+u/d/f/b g/G wheel scroll logs • o opens log • q exits",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    }

    fn info_panel(&self) -> Paragraph<'_> {
        let job = &self.jobs[self.selected];
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

    fn log_view(&self, pipeline_finished: bool, height: u16) -> Paragraph<'_> {
        let job = &self.jobs[self.selected];
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

        let max_lines = height.saturating_sub(2).max(1) as usize;
        lines.extend(job.visible_logs(max_lines));

        let mut title = "Logs".to_string();
        if job.status.is_done() {
            title.push_str(" (complete)");
        }

        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false })
    }

    fn current_log_path(&self) -> PathBuf {
        self.jobs[self.selected].log_path.clone()
    }

    fn set_status(&mut self, name: &str, status: UiJobStatus) {
        if let Some(idx) = self.order.get(name).copied() {
            self.jobs[idx].status = status;
            if status == UiJobStatus::Running && self.selected == 0 {
                self.selected = idx;
            }
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
        if self.jobs.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.jobs.len();
        self.jobs[self.selected].auto_follow();
    }

    fn previous_job(&mut self) {
        if self.jobs.is_empty() {
            return;
        }
        if self.selected == 0 {
            self.selected = self.jobs.len() - 1;
        } else {
            self.selected -= 1;
        }
        self.jobs[self.selected].auto_follow();
    }

    fn scroll_logs_line_up(&mut self) {
        self.jobs[self.selected].scroll_lines_up(LOG_SCROLL_STEP);
    }

    fn scroll_logs_line_down(&mut self) {
        self.jobs[self.selected].scroll_lines_down(LOG_SCROLL_STEP);
    }

    fn scroll_logs_half_up(&mut self) {
        self.jobs[self.selected].scroll_lines_up(LOG_SCROLL_HALF);
    }

    fn scroll_logs_half_down(&mut self) {
        self.jobs[self.selected].scroll_lines_down(LOG_SCROLL_HALF);
    }

    fn scroll_logs_page_up(&mut self) {
        self.jobs[self.selected].scroll_lines_up(LOG_SCROLL_PAGE);
    }

    fn scroll_logs_page_down(&mut self) {
        self.jobs[self.selected].scroll_lines_down(LOG_SCROLL_PAGE);
    }

    fn scroll_logs_mouse_up(&mut self) {
        self.jobs[self.selected].scroll_lines_up(LOG_SCROLL_STEP);
    }

    fn scroll_logs_mouse_down(&mut self) {
        self.jobs[self.selected].scroll_lines_down(LOG_SCROLL_STEP);
    }

    fn scroll_top(&mut self) {
        self.jobs[self.selected].scroll_to_top();
    }

    fn scroll_bottom(&mut self) {
        self.jobs[self.selected].scroll_to_bottom();
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
    logs: VecDeque<String>,
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
            logs: VecDeque::new(),
            scroll_offset: 0,
            follow_logs: true,
        }
    }

    fn push_log(&mut self, line: String) {
        if self.logs.len() == LOG_CAPACITY {
            self.logs.pop_front();
        }
        self.logs.push_back(line);
        if self.follow_logs {
            self.scroll_offset = 0;
        }
    }

    fn auto_follow(&mut self) {
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

    fn visible_logs(&self, max_lines: usize) -> Vec<Line<'static>> {
        if self.logs.is_empty() {
            return vec![Line::from("(no output yet)")];
        }

        let end = self.logs.len().saturating_sub(self.scroll_offset);
        let window = max_lines.max(1);
        let start = end.saturating_sub(window);
        self.logs
            .iter()
            .skip(start)
            .take(end - start)
            .map(|line| format_log_entry(line))
            .collect()
    }
}

enum ScrollDirection {
    Up,
    Down,
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

    fn label(self) -> &'static str {
        match self {
            UiJobStatus::Pending => "pending",
            UiJobStatus::Running => "running",
            UiJobStatus::Success => "success",
            UiJobStatus::Failed => "failed",
            UiJobStatus::Skipped => "skipped",
        }
    }
}

fn format_log_entry(line: &str) -> Line<'static> {
    if let Some(rest) = line.strip_prefix('[') {
        if let Some(idx) = rest.find("] ") {
            let meta = &rest[..idx];
            let remainder = &rest[idx + 2..];
            if let Some(space_idx) = meta.rfind(' ') {
                let (timestamp, number) = meta.split_at(space_idx);
                let number = number.trim();
                return Line::from(vec![
                    Span::raw("[".to_string()),
                    Span::styled(timestamp.to_string(), Style::default().fg(Color::Blue)),
                    Span::raw(" ".to_string()),
                    Span::styled(number.to_string(), Style::default().fg(Color::Green)),
                    Span::raw("] ".to_string()),
                    Span::raw(remainder.to_string()),
                ]);
            }
        }
    }

    Line::from(line.to_string())
}
