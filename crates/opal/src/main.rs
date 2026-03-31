use anyhow::Result;
use opal::Cli;
use opal::McpArgs;
use opal::app::OpalApp;
use std::env;
use std::io::{self, IsTerminal};
use structopt::StructOpt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::Subscriber;

#[tokio::main]
async fn main() -> Result<()> {
    if maybe_handle_mcp_meta_flag()? {
        return Ok(());
    }
    let opts = Cli::from_args();
    install_logging(opts.log_level)?;
    OpalApp::from_current_dir()?.run_cli(opts).await
}

fn maybe_handle_mcp_meta_flag() -> Result<bool> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [subcommand, flag] if subcommand == "mcp" && (flag == "--help" || flag == "-h") => {
            print!("{}", mcp_help_text()?);
            Ok(true)
        }
        [subcommand, flag]
            if subcommand == "mcp" && (flag == "--version" || flag == "-V") =>
        {
            println!("opal mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn mcp_help_text() -> Result<String> {
    let mut buffer = Vec::new();
    let app = McpArgs::clap().bin_name("opal mcp");
    app.write_help(&mut buffer)?;
    let mut text = String::from_utf8(buffer)?;
    if let Some(first_line) = text.lines().next()
        && first_line.starts_with("opal-mcp ")
    {
        text = text.replacen("opal-mcp ", "opal mcp ", 1);
    }
    Ok(text)
}

fn install_logging(level: tracing::Level) -> Result<()> {
    let env_filter = EnvFilter::new(level.as_str());
    let use_ansi = io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    let subscriber = Subscriber::builder()
        .with_ansi(use_ansi)
        .with_env_filter(env_filter)
        .with_writer(std::io::stdout)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(())
}
