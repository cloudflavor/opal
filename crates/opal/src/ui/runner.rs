use std::collections::HashMap;
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
use ratatui::widgets::{Block, Borders, Clear, Scrollbar, ScrollbarOrientation};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::history::HistoryEntry;
use crate::{ai, config::OpalConfig, naming::job_name_slug, runtime, secrets::SecretsStore};

use super::state::{
    LogFilter, UiState, page_file_with_pager, page_log_with_colors, page_text_with_pager,
};
use super::types::{HistoryAction, UiCommand, UiEvent, UiJobInfo, UiJobResources};

pub(super) struct UiRunner {
    tx: UnboundedSender<UiEvent>,
    rx: UnboundedReceiver<UiEvent>,
    commands: UnboundedSender<UiCommand>,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    state: UiState,
    pipeline_finished: bool,
    exit_requested: bool,
    abort_sent: bool,
    pending_history_preview: Option<PathBuf>,
}

impl UiRunner {
    // TODO: do not use skip clippy macros
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        jobs: Vec<UiJobInfo>,
        history: Vec<HistoryEntry>,
        current_run_id: String,
        job_resources: HashMap<String, UiJobResources>,
        plan_text: String,
        workdir: PathBuf,
        pipeline_path: PathBuf,
        tx: UnboundedSender<UiEvent>,
        rx: UnboundedReceiver<UiEvent>,
        commands: UnboundedSender<UiCommand>,
    ) -> Result<Self> {
        let stdout = io::stdout();
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("failed to create terminal")?;
        Ok(Self {
            tx,
            rx,
            commands,
            terminal,
            state: UiState::new(
                jobs,
                history,
                current_run_id,
                job_resources,
                plan_text,
                workdir,
                pipeline_path,
            ),
            pipeline_finished: false,
            exit_requested: false,
            abort_sent: false,
            pending_history_preview: None,
        })
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
        if self.exit_requested {
            self.pipeline_finished
        } else {
            false
        }
    }

    fn request_abort(&mut self) {
        if self.abort_sent {
            return;
        }
        let _ = self.commands.send(UiCommand::AbortPipeline);
        self.abort_sent = true;
    }

    fn draw(&mut self) -> Result<()> {
        // TODO: does too much - refactor
        self.terminal.draw(|frame| {
            let mut constraints = Vec::new();
            let mut history_idx = None;
            let mut yaml_idx = None;
            if self.state.history_pane_visible() {
                history_idx = Some(constraints.len());
                constraints.push(Constraint::Length(36));
            }
            let main_idx = constraints.len();
            constraints.push(Constraint::Min(0));
            if self.state.job_yaml_pane_visible() {
                yaml_idx = Some(constraints.len());
                constraints.push(Constraint::Length(52));
            }
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(constraints)
                .split(frame.area());

            if let Some(idx) = history_idx {
                let history_area = columns[idx];
                let (history_list, mut history_scrollbar) =
                    self.state.history_widget(history_area.height);
                frame.render_widget(history_list, history_area);
                frame.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight),
                    history_area,
                    &mut history_scrollbar,
                );
            }

            let main_area = columns[main_idx];
            let tab_width = main_area.width.saturating_sub(2).max(1);
            let (tabs, tab_height) = self.state.tabs(tab_width);

            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(tab_height),
                    Constraint::Length(self.state.info_panel_height()),
                    Constraint::Min(0),
                    Constraint::Length(4),
                ])
                .split(main_area);

            frame.render_widget(tabs, layout[0]);

            let info = self.state.info_panel();
            frame.render_widget(info, layout[1]);

            let log_widget =
                self.state
                    .log_view(self.pipeline_finished, layout[2].width, layout[2].height);
            frame.render_widget(log_widget, layout[2]);
            frame.render_widget(self.state.help_prompt(layout[3].width), layout[3]);

            if let Some(idx) = yaml_idx {
                frame.render_widget(self.state.job_yaml_panel(), columns[idx]);
            }

            if self.state.help_visible() {
                let area = centered_rect(60, 80, frame.area());
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
                self.state
                    .update_help_viewport(help_layout[1].width, help_layout[1].height);
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
                UiEvent::AnalysisStarted { name, provider } => {
                    self.state.analysis_started(&name, &provider)
                }
                UiEvent::AnalysisChunk { name, delta } => self.state.analysis_chunk(&name, &delta),
                UiEvent::AnalysisFinished {
                    name,
                    final_text,
                    saved_path,
                    error,
                } => self
                    .state
                    .analysis_finished(&name, final_text, saved_path, error),
                UiEvent::AiPromptReady { name, prompt } => {
                    self.state.ai_prompt_ready(&name, prompt)
                }
                UiEvent::HistoryPreviewReady { title, path, lines } => {
                    self.pending_history_preview = Some(path.clone());
                    self.state.set_history_preview_lines(title, &path, lines);
                }
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

    // TODO: does way too much, separate concerns
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
                    self.state.scroll_help(-1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.state.scroll_help(1);
                }
                KeyCode::PageUp => {
                    self.state.scroll_help_page_up();
                }
                KeyCode::PageDown => {
                    self.state.scroll_help_page_down();
                }
                KeyCode::Home => {
                    self.state.scroll_help_to_top();
                }
                KeyCode::End => {
                    self.state.scroll_help_to_bottom();
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
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.view_plan()?;
                return Ok(());
            }
            KeyCode::Char('H') => {
                self.state.toggle_history_pane();
                return Ok(());
            }
            KeyCode::Char('Y') => {
                self.state.toggle_job_yaml_pane();
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
                                self.spawn_history_preview_load(title, path);
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
                                } else {
                                    self.refresh_history_preview();
                                }
                            }
                            HistoryAction::ViewHistoryJob { run_id, job_name } => {
                                if let Err(err) = self.state.view_history_job(&run_id, &job_name) {
                                    let title = format!("{run_id} • {job_name}");
                                    let empty = PathBuf::new();
                                    self.state.set_history_preview_message(
                                        title,
                                        &empty,
                                        err.to_string(),
                                    );
                                } else {
                                    self.refresh_history_preview();
                                }
                            }
                            HistoryAction::ViewDir { title, path } => {
                                self.spawn_directory_preview_load(title, path);
                            }
                            HistoryAction::ViewFile { title, path } => {
                                if let Err(err) = self.suspend_terminal(|| {
                                    page_file_with_pager(title.as_str(), &path)
                                }) {
                                    self.state.set_history_preview_message(
                                        title,
                                        &path,
                                        format!("failed to open file: {err}"),
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

        if self.state.focus_is_job_yaml() {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
                    self.state.scroll_job_yaml_line_down()
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
                    self.state.scroll_job_yaml_line_up()
                }
                KeyCode::PageDown => self.state.scroll_job_yaml_page_down(24),
                KeyCode::PageUp => self.state.scroll_job_yaml_page_up(24),
                KeyCode::Char('y') => self.view_current_job_yaml()?,
                _ => {}
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Char('J') => {
                self.state.next_job();
                self.refresh_history_preview();
            }
            KeyCode::Char('k') | KeyCode::Char('K') => {
                self.state.previous_job();
                self.refresh_history_preview();
            }
            KeyCode::Char('h') => {
                self.state.previous_job();
                self.refresh_history_preview();
            }
            KeyCode::Char('l') => {
                self.state.next_job();
                self.refresh_history_preview();
            }
            KeyCode::Left => {
                self.state.previous_job();
                self.refresh_history_preview();
            }
            KeyCode::Right => {
                self.state.next_job();
                self.refresh_history_preview();
            }
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
                    self.refresh_history_preview();
                }
            }
            KeyCode::Up => {
                if modifiers.contains(KeyModifiers::SHIFT)
                    || modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.state.scroll_logs_line_up();
                } else {
                    self.state.previous_job();
                    self.refresh_history_preview();
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
            KeyCode::Char('a') => {
                if let Some((name, source_name)) = self.state.analysis_action_request() {
                    if self.state.history_view_active() {
                        let handle = tokio::runtime::Handle::current();
                        if let Some(snapshot) = handle.block_on(self.state.analysis_snapshot()) {
                            self.spawn_local_analysis(snapshot, false);
                        }
                    } else {
                        let _ = self
                            .commands
                            .send(UiCommand::AnalyzeJob { name, source_name });
                    }
                }
            }
            KeyCode::Char('A') => {
                if self.state.toggle_ai_prompt_preview() {
                    return Ok(());
                }
                if let Some((name, source_name)) = self.state.ai_prompt_preview_request() {
                    if self.state.history_view_active() {
                        let handle = tokio::runtime::Handle::current();
                        if let Some(snapshot) = handle.block_on(self.state.analysis_snapshot()) {
                            self.spawn_local_analysis(snapshot, true);
                        }
                    } else {
                        let _ = self
                            .commands
                            .send(UiCommand::PreviewAiPrompt { name, source_name });
                    }
                }
            }
            KeyCode::Char('y') => {
                self.view_current_job_yaml()?;
            }
            KeyCode::Char('0') => self.state.set_log_filter(LogFilter::All),
            KeyCode::Char('1') => self.state.set_log_filter(LogFilter::Errors),
            KeyCode::Char('2') => self.state.set_log_filter(LogFilter::Warnings),
            KeyCode::Char('3') => self.state.set_log_filter(LogFilter::Downloads),
            KeyCode::Char('4') => self.state.set_log_filter(LogFilter::Build),
            KeyCode::Char('c') => self.state.cycle_tab_density(),
            _ => {}
        }

        Ok(())
    }

    fn refresh_history_preview(&mut self) {
        let Some((title, path)) = self.state.desired_history_preview() else {
            self.pending_history_preview = None;
            return;
        };
        if self.pending_history_preview.as_ref() == Some(&path) {
            return;
        }
        self.spawn_history_preview_load(title, path);
    }

    fn spawn_history_preview_load(&mut self, title: String, path: PathBuf) {
        self.pending_history_preview = Some(path.clone());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let lines = match UiState::load_history_preview_lines(&path).await {
                Ok(lines) => lines,
                Err(err) => vec![format!("failed to load log: {err}")],
            };
            let _ = tx.send(UiEvent::HistoryPreviewReady { title, path, lines });
        });
    }

    fn spawn_directory_preview_load(&mut self, title: String, path: PathBuf) {
        self.pending_history_preview = Some(path.clone());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let lines = match UiState::load_directory_preview_lines(&path).await {
                Ok(lines) => lines,
                Err(err) => vec![format!("failed to read directory: {err}")],
            };
            let _ = tx.send(UiEvent::HistoryPreviewReady { title, path, lines });
        });
    }

    fn handle_mouse(&mut self, event: MouseEvent) {
        if self.state.help_visible() {
            match event.kind {
                MouseEventKind::ScrollUp => self.state.scroll_help(-1),
                MouseEventKind::ScrollDown => self.state.scroll_help(1),
                _ => {}
            }
            return;
        }

        match event.kind {
            MouseEventKind::ScrollUp => {
                if self.state.focus_is_history() {
                    self.state.history_move_up();
                } else if self.state.focus_is_job_yaml() {
                    self.state.scroll_job_yaml_line_up();
                } else {
                    self.state.scroll_logs_mouse_up();
                }
            }
            MouseEventKind::ScrollDown => {
                if self.state.focus_is_history() {
                    self.state.history_move_down();
                } else if self.state.focus_is_job_yaml() {
                    self.state.scroll_job_yaml_line_down();
                } else {
                    self.state.scroll_logs_mouse_down();
                }
            }
            _ => {}
        }
    }

    fn view_current_log(&mut self) -> Result<()> {
        if let Some(text) = self.state.current_analysis_text() {
            return self.suspend_terminal(|| page_text_with_pager(&text));
        }
        if let Some(path) = self.state.current_log_path() {
            self.suspend_terminal(|| page_log_with_colors(&path))
        } else {
            Ok(())
        }
    }

    fn view_plan(&mut self) -> Result<()> {
        let plan = self.state.plan_text();
        self.suspend_terminal(|| page_text_with_pager(&plan))
    }

    fn view_current_job_yaml(&mut self) -> Result<()> {
        let yaml = self.state.job_yaml_text_for_pager();
        self.suspend_terminal(|| page_text_with_pager(&yaml))
    }

    fn spawn_local_analysis(&self, snapshot: super::state::AiAnalysisSnapshot, preview_only: bool) {
        let tx = self.tx.clone();
        let workdir = self.state.workdir().to_path_buf();
        let handle = tokio::runtime::Handle::current();
        handle.spawn(async move {
            let job_name = snapshot.job_name.clone();
            let analysis_tx = tx.clone();
            let result: Result<()> = async move {
                let settings = OpalConfig::load_async(&workdir).await?;
                let secrets = SecretsStore::load_async(&workdir).await?;
                let provider = settings
                    .ai_settings()
                    .default_provider
                    .unwrap_or(crate::config::AiProviderConfig::Ollama);
                let provider_kind = match provider {
                    crate::config::AiProviderConfig::Ollama => ai::AiProviderKind::Ollama,
                    crate::config::AiProviderConfig::Claude => ai::AiProviderKind::Claude,
                    crate::config::AiProviderConfig::Codex => ai::AiProviderKind::Codex,
                };
                let provider_label = match provider_kind {
                    ai::AiProviderKind::Ollama => "ollama",
                    ai::AiProviderKind::Claude => "claude",
                    ai::AiProviderKind::Codex => "codex",
                };
                let context = ai::AiContext {
                    job_name: snapshot.job_name.clone(),
                    source_name: snapshot.source_name.clone(),
                    stage: snapshot.stage.clone(),
                    job_yaml: snapshot.job_yaml,
                    runner_summary: snapshot.runner_summary,
                    pipeline_summary: snapshot.pipeline_summary,
                    runtime_summary: snapshot.runtime_summary,
                    log_excerpt: snapshot.log_excerpt,
                    failure_hint: snapshot.failure_hint,
                };
                let rendered = ai::render_job_analysis_prompt_async(
                    &workdir,
                    settings.ai_settings(),
                    &context,
                )
                .await?;
                if preview_only {
                    let mut text = String::new();
                    if let Some(system) = rendered.system {
                        text.push_str("# System\n\n");
                        text.push_str(system.trim());
                        text.push_str("\n\n");
                    }
                    text.push_str("# Prompt\n\n");
                    text.push_str(rendered.prompt.trim());
                    let _ = analysis_tx.send(UiEvent::AiPromptReady {
                        name: snapshot.job_name,
                        prompt: text,
                    });
                    return Ok(());
                }

                let prompt = secrets.mask_fragment(&rendered.prompt).into_owned();
                let system = rendered
                    .system
                    .as_deref()
                    .map(|text| secrets.mask_fragment(text).into_owned());
                let save_path = settings.ai_settings().save_analysis.then(|| {
                    runtime::session_dir(&snapshot.run_id)
                        .join(job_name_slug(&snapshot.job_name))
                        .join("analysis")
                        .join(format!("{provider_label}.md"))
                });
                let _ = analysis_tx.send(UiEvent::AnalysisStarted {
                    name: snapshot.job_name.clone(),
                    provider: provider_label.to_string(),
                });
                let request = ai::AiRequest {
                    provider: provider_kind,
                    prompt,
                    system,
                    host: (provider_kind == ai::AiProviderKind::Ollama)
                        .then(|| settings.ai_settings().ollama.host.clone()),
                    model: match provider_kind {
                        ai::AiProviderKind::Ollama => {
                            Some(settings.ai_settings().ollama.model.clone())
                        }
                        ai::AiProviderKind::Codex => settings.ai_settings().codex.model.clone(),
                        ai::AiProviderKind::Claude => None,
                    },
                    command: (provider_kind == ai::AiProviderKind::Codex)
                        .then(|| settings.ai_settings().codex.command.clone()),
                    args: Vec::new(),
                    workdir: (provider_kind == ai::AiProviderKind::Codex)
                        .then_some(workdir.clone()),
                    save_path: save_path.clone(),
                };
                let result = ai::analyze_with_default_provider(&request, |chunk| {
                    let ai::AiChunk::Text(text) = chunk;
                    let _ = analysis_tx.send(UiEvent::AnalysisChunk {
                        name: snapshot.job_name.clone(),
                        delta: text,
                    });
                })
                .await
                .map_err(|err| anyhow::anyhow!(err.message))?;
                if let Some(path) = &save_path {
                    if let Some(parent) = path.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    tokio::fs::write(path, &result.text).await?;
                }
                let _ = analysis_tx.send(UiEvent::AnalysisFinished {
                    name: snapshot.job_name,
                    final_text: result.text,
                    saved_path: save_path,
                    error: None,
                });
                Ok(())
            }
            .await;

            if let Err(err) = result {
                let _ = tx.send(UiEvent::AnalysisFinished {
                    name: job_name,
                    final_text: String::new(),
                    saved_path: None,
                    error: Some(err.to_string()),
                });
            }
        });
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
