//! RoomKit's development application: configuration and assembly live here.

mod accounts;
mod agent;
mod application;
mod branding;
mod command_env;
mod compaction;
mod config;
mod delegation;
mod diagnostics;
mod digest;
mod docs;
mod exit_summary;
mod files;
mod herdr;
mod hooks;
mod http_client;
mod images;
mod instructions;
mod permissions;
mod plans;
mod processes;
mod profiles;
mod services;
mod sessions;
mod shell;
mod storage;
mod sync;
#[cfg(test)]
mod testing;
mod timeouts;
mod tool_views;
mod updates;

use clap::Parser;
use std::{error::Error, io::IsTerminal, process::ExitCode};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::main]
async fn main() -> ExitCode {
    match launch().await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("roomkit-code: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn launch() -> Result<ExitCode> {
    let cli = config::Cli::parse();
    if updates::cli(&cli).await? {
        return Ok(ExitCode::SUCCESS);
    }
    let settings = config::Settings::load(&cli)?;
    if cli.check_config {
        println!("Launch configuration valid. Provider and agent profiles are loaded at startup.");
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(prompt) = application::headless::Prompt::from_cli(&cli)? {
        return application::headless::run(settings, prompt).await;
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err("Interactive mode requires a terminal; use --help for options".into());
    }
    application::run(settings).await?;
    Ok(ExitCode::SUCCESS)
}
