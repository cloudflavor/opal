use super::runner::UiRunner;
use super::types::{UiCommand, UiEvent, UiJobInfo, UiJobResources, UiJobStatus};
use crate::history::HistoryEntry;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

pub struct UiHandle {
    sender: UnboundedSender<UiEvent>,
    command_rx: Mutex<Option<UnboundedReceiver<UiCommand>>>,
    thread: thread::JoinHandle<()>,
}

#[derive(Clone)]
pub struct UiBridge {
    sender: UnboundedSender<UiEvent>,
}

impl UiHandle {
    pub fn start(
        jobs: Vec<UiJobInfo>,
        history: Vec<HistoryEntry>,
        current_run_id: String,
        job_resources: HashMap<String, UiJobResources>,
        plan_text: String,
        workdir: PathBuf,
        pipeline_path: PathBuf,
    ) -> Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let thread_tx = tx.clone();
        let handle = thread::spawn(move || {
            match UiRunner::new(
                jobs,
                history,
                current_run_id,
                job_resources,
                plan_text,
                workdir,
                pipeline_path,
                rx,
                cmd_tx,
            ) {
                Ok(runner) => {
                    if let Err(err) = runner.run() {
                        eprintln!("ui error: {err:?}");
                    }
                }
                Err(err) => {
                    eprintln!("ui error: {err:?}");
                }
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

    pub fn job_manual_pending(&self, name: &str) {
        let _ = self.sender.send(UiEvent::JobManual {
            name: name.to_string(),
        });
    }
}
