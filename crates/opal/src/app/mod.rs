pub(crate) mod context;
pub(crate) mod plan;
pub(crate) mod run;
pub(crate) mod view;

use crate::{Cli, Commands};
use anyhow::Result;
use std::env;
use std::path::PathBuf;

#[derive(Clone)]
pub struct OpalApp {
    current_dir: PathBuf,
}

impl OpalApp {
    pub fn from_current_dir() -> Result<Self> {
        Ok(Self {
            current_dir: env::current_dir()?,
        })
    }

    pub async fn run_cli(&self, cli: Cli) -> Result<()> {
        self.run_command(cli.commands).await
    }

    pub async fn run_command(&self, command: Commands) -> Result<()> {
        match command {
            Commands::Run(args) => run::execute(self, args).await,
            Commands::Plan(args) => plan::execute(self, args),
            Commands::View(args) => view::execute(self, args).await,
            Commands::Mcp(_) => crate::mcp::serve_stdio().await,
        }
    }

    pub(crate) fn resolve_workdir(&self, workdir: Option<PathBuf>) -> PathBuf {
        workdir.unwrap_or_else(|| self.current_dir.clone())
    }
}
