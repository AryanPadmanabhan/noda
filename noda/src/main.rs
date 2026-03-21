mod agent;
mod api;
mod db;
mod executors;
mod server;
mod types;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Parser)]
#[command(name = "noda")]
#[command(about = "OTA orchestrator and agent for customer-owned artifacts")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the control-plane API server.
    Server {
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: String,
        #[arg(long, default_value = "noda.db")]
        db: PathBuf,
    },
    /// Run the agent loop for a single asset.
    Agent {
        #[arg(long)]
        server: String,
        #[arg(long)]
        asset_id: String,
        #[arg(long)]
        asset_type: String,
        #[arg(long, default_value = "idle")]
        mission_state: String,
        #[arg(long, default_value_t = 15)]
        poll_seconds: u64,
        #[arg(long, default_value = "./agent-state")]
        state_dir: PathBuf,
        #[arg(long)]
        labels: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Server { bind, db } => server::run(bind, db).await,
        Commands::Agent {
            server,
            asset_id,
            asset_type,
            mission_state,
            poll_seconds,
            state_dir,
            labels,
        } => {
            let cfg = agent::AgentConfig {
                server,
                asset_id,
                asset_type,
                mission_state,
                poll_seconds,
                state_dir,
                labels,
            };
            agent::run(cfg).await
        }
    }
}
