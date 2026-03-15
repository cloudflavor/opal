use std::io::{self, Stdout};
use std::path::PathBuf;
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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Block, Borders, Clear};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::history::HistoryEntry;

use super::state::{UiState, page_log_with_colors};
use super::types::{HistoryAction, UiCommand, UiEvent, UiJobInfo};

pub(super) struct UiRunner {
    rx: UnboundedReceiver<UiEvent>,
    commands: UnboundedSender<UiCommand>,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    state: UiState,
    pipeline_finished: bool,
    exit_requested: bool,
    abort_sent: bool,
}

impl UiRunner {
    pub(super) fn new(
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
            abort_sent: false,
        }
    }

    pub(super) fn run(mut self) -> Result<()> {
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

    fn request_abort(&mut self) {
        if self.abort_sent {
            return;
        }
        let _ = self.commands.send(UiCommand::AbortPipeline);
        self.abort_sent = true;
    }

    fn draw(&mut self) -> Result<()> {
        self.terminal.draw(|frame| {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(32), Constraint::Min(0)])
                .split(frame.size());

            let history_split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(columns[0]);
            let history = self.state.history_widget(history_split[0].height);
            frame.render_widget(history, history_split[0]);
            frame.render_widget(self.state.help_prompt(), history_split[1]);

            let tab_width = columns[1].width.saturating_sub(2).max(1);
            let (tabs, tab_height) = self.state.tabs(tab_width);

            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(tab_height),
                    Constraint::Length(4),
                    Constraint::Min(0),
                ])
                .split(columns[1]);

            frame.render_widget(tabs, layout[0]);

            let info = self.state.info_panel();
            frame.render_widget(info, layout[1]);

            let log_widget =
                self.state
                    .log_view(self.pipeline_finished, layout[2].width, layout[2].height);
            frame.render_widget(log_widget, layout[2]);

            if self.state.help_visible() {
                let area = centered_rect(60, 80, frame.size());
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(self.state.help_window_title());
                let inner = block.inner(area);
                let help_layout = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(4),
                        Constraint::Min(0),
                        Constraint::Length(3),
                    ])
                    .split(inner);
                frame.render_widget(Clear, area);
                frame.render_widget(block, area);
                frame.render_widget(self.state.help_header(), help_layout[0]);
                frame.render_widget(self.state.help_body(), help_layout[1]);
                frame.render_widget(self.state.help_footer(), help_layout[2]);
            }
        })?;
        Ok(())
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                UiEvent::JobStarted { name } => {
                    self.pipeline_finished = false;
                    self.state.job_started(&name);
                }
                UiEvent::JobRestarted { name } => {
                    self.pipeline_finished = false;
                    self.state.restart_job(&name);
                }
                UiEvent::JobLog { name, line } => self.state.push_log(&name, line),
                UiEvent::JobFinished {
                    name,
                    status,
                    duration,
                    error,
                } => self.state.finish_job(&name, status, duration, error),
                UiEvent::JobManual { name } => self.state.set_manual_pending(&name, true),
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
        if self.state.help_visible() {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc => {
                    self.state.toggle_help();
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    self.state.show_help_shortcuts();
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    self.state.open_help_document_digit(c);
                }
                KeyCode::Left => {
                    self.state.previous_help_document();
                }
                KeyCode::Right => {
                    self.state.next_help_document();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.state.scroll_help_document(-1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.state.scroll_help_document(1);
                }
                KeyCode::PageUp => {
                    self.state.scroll_help_document_page_up();
                }
                KeyCode::PageDown => {
                    self.state.scroll_help_document_page_down();
                }
                KeyCode::Home => {
                    self.state.scroll_help_doc_to_top();
                }
                KeyCode::End => {
                    self.state.scroll_help_doc_to_bottom();
                }
                _ => {}
            }
            return Ok(());
        }
        match key.code {
            KeyCode::Char('q') => {
                self.exit_requested = true;
                self.request_abort();
                return Ok(());
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.exit_requested = true;
                self.request_abort();
                return Ok(());
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.state.toggle_focus();
                return Ok(());
            }
            KeyCode::Char('?') => {
                self.state.toggle_help();
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
            KeyCode::Char('x') => {
                if let Some(name) = self.state.cancelable_job_name() {
                    let _ = self.commands.send(UiCommand::CancelJob { name });
                }
            }
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
            KeyCode::Char('m') => {
                if let Some(name) = self.state.manual_job_name() {
                    let _ = self.commands.send(UiCommand::StartManual { name });
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

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let center = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(chunks[1]);
    center[1]
}
