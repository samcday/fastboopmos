use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use fastboopmos::cli::{Cli, Command};
use fastboopmos::cmd;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    let http = reqwest::Client::builder()
        .build()
        .context("failed to build reqwest client")?;

    match &cli.command {
        Command::List(args) => cmd::list::run(&http, &cli, args).await,
        Command::Build(args) => cmd::build::run(&http, &cli, args).await,
        Command::Channel(args) => cmd::channel::run(&http, &cli, args).await,
    }
}
