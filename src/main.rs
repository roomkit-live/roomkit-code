//! RoomKit's development application: configuration and assembly live here.

mod accounts;
mod agent;
mod application;
mod branding;
mod command_env;
mod config;
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
mod timeouts;
mod tool_views;
mod updates;

use clap::Parser;
use std::{error::Error, io::IsTerminal, process::ExitCode};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::main]
async fn main() -> ExitCode {
    match launch().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("roomkit-code: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn launch() -> Result<()> {
    let cli = config::Cli::parse();
    if updates::cli(&cli).await? {
        return Ok(());
    }
    let settings = config::Settings::load(&cli)?;
    if cli.check_config {
        println!("Launch configuration valid. Provider and agent profiles are loaded at startup.");
        return Ok(());
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err("Interactive mode requires a terminal; use --help for options".into());
    }
    application::run(settings).await
}
