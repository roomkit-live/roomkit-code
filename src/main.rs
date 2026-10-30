//! RoomKit's development application: configuration and assembly live here.

mod agent;
mod application;
mod config;
mod permissions;
mod profiles;
mod sessions;
mod storage;
mod tool_views;

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
