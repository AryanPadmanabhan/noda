mod state;
mod validation;
mod workflow;

use crate::types::*;
use anyhow::Result;
use reqwest::Client;
use std::{fs, path::PathBuf, time::Duration};
use tokio::time::sleep;
use tracing::error;

use state::{load_state, save_state, LocalState};
use workflow::{execute_command, resume_pending_boot, CommandExecution};

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub server: String,
    pub asset_id: String,
    pub asset_type: String,
    pub mission_state: String,
    pub poll_seconds: u64,
    pub state_dir: PathBuf,
    pub labels: Vec<String>,
}

pub async fn run(cfg: AgentConfig) -> Result<()> {
    fs::create_dir_all(&cfg.state_dir)?;
    let client = Client::builder().timeout(Duration::from_secs(60)).build()?;

    loop {
        let mut state = load_state(&cfg.state_dir)?;
        resume_pending_boot(&client, &cfg, &mut state).await?;
        save_state(&cfg.state_dir, &state)?;

        checkin(&client, &cfg, &state).await?;
        if state.pending_boot.is_some() {
            sleep(Duration::from_secs(cfg.poll_seconds)).await;
            continue;
        }

        let polled = poll(&client, &cfg).await?;
        if polled.commands.is_empty() {
            sleep(Duration::from_secs(cfg.poll_seconds)).await;
            continue;
        }

        for cmd in polled.commands {
            match execute_command(&client, &cfg, &cmd).await {
                Ok(CommandExecution::Completed {
                    message,
                    state: new_state,
                }) => {
                    save_state(&cfg.state_dir, &new_state)?;
                    report_result(
                        &client,
                        &cfg,
                        AgentResultRequest {
                            command_id: cmd.id,
                            asset_id: cfg.asset_id.clone(),
                            success: true,
                            message,
                            active_slot: new_state.active_slot,
                            booted_version: new_state.current_version,
                        },
                    )
                    .await?;
                }
                Ok(CommandExecution::Deferred { state: new_state }) => {
                    save_state(&cfg.state_dir, &new_state)?;
                }
                Err(err) => {
                    error!(error = %err, asset_id = %cfg.asset_id, "command failed");
                    report_result(
                        &client,
                        &cfg,
                        AgentResultRequest {
                            command_id: cmd.id,
                            asset_id: cfg.asset_id.clone(),
                            success: false,
                            message: err.to_string(),
                            active_slot: state.active_slot.clone(),
                            booted_version: state.current_version.clone(),
                        },
                    )
                    .await?;
                }
            }
        }

        sleep(Duration::from_secs(cfg.poll_seconds)).await;
    }
}

async fn checkin(client: &Client, cfg: &AgentConfig, state: &LocalState) -> Result<()> {
    let req = AgentCheckinRequest {
        asset_id: cfg.asset_id.clone(),
        asset_type: cfg.asset_type.clone(),
        mission_state: cfg.mission_state.clone(),
        labels: cfg.labels.clone(),
        current_version: state.current_version.clone(),
        active_slot: state.active_slot.clone(),
        status: Some(AssetStatus::Online),
    };
    client
        .post(format!("{}/v1/agent/checkin", cfg.server))
        .json(&req)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn poll(client: &Client, cfg: &AgentConfig) -> Result<AgentPollResponse> {
    let resp = client
        .post(format!("{}/v1/agent/poll", cfg.server))
        .json(&AgentPollRequest {
            asset_id: cfg.asset_id.clone(),
        })
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.json().await?)
}

async fn report_result(
    client: &Client,
    cfg: &AgentConfig,
    result: AgentResultRequest,
) -> Result<()> {
    client
        .post(format!("{}/v1/agent/result", cfg.server))
        .json(&result)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
