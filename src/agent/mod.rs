use crate::{executors, types::*};
use anyhow::{anyhow, Context, Result};
use reqwest::{header::{HeaderMap, HeaderName, HeaderValue}, Client};
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}, process::Command as ProcessCommand, time::Duration};
use tokio::time::sleep;
use tracing::{error, info, warn};
use url::Url;

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

#[derive(Debug, serde::Serialize, serde::Deserialize, Default)]
struct LocalState {
    current_version: Option<String>,
    active_slot: Option<String>,
}

pub async fn run(cfg: AgentConfig) -> Result<()> {
    fs::create_dir_all(&cfg.state_dir)?;
    let client = Client::builder().timeout(Duration::from_secs(60)).build()?;

    loop {
        let state = load_state(&cfg.state_dir)?;
        checkin(&client, &cfg, &state).await?;
        let polled = poll(&client, &cfg).await?;

        if polled.commands.is_empty() {
            sleep(Duration::from_secs(cfg.poll_seconds)).await;
            continue;
        }

        for cmd in polled.commands {
            let result = execute_command(&client, &cfg, &cmd).await;
            match result {
                Ok((message, local_state)) => {
                    save_state(&cfg.state_dir, &local_state)?;
                    report_result(
                        &client,
                        &cfg,
                        AgentResultRequest {
                            command_id: cmd.id,
                            asset_id: cfg.asset_id.clone(),
                            success: true,
                            message,
                            active_slot: local_state.active_slot,
                            booted_version: local_state.current_version,
                        },
                    )
                    .await?;
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
        status: Some("online".into()),
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
        .json(&AgentPollRequest { asset_id: cfg.asset_id.clone() })
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.json().await?)
}

async fn report_result(client: &Client, cfg: &AgentConfig, result: AgentResultRequest) -> Result<()> {
    client
        .post(format!("{}/v1/agent/result", cfg.server))
        .json(&result)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

async fn execute_command(client: &Client, cfg: &AgentConfig, cmd: &CommandRecord) -> Result<(String, LocalState)> {
    info!(asset_id = %cfg.asset_id, command_id = %cmd.id, release = %cmd.release_version, "executing command");
    let artifact_path = download_artifact(client, &cfg.state_dir, &cmd.manifest.artifact).await?;
    verify_sha256(&artifact_path, cmd.manifest.artifact.sha256.as_deref())?;

    let mut state = load_state(&cfg.state_dir)?;
    let executor = executors::build(&cmd.manifest.install.executor);
    let current_slot = state.active_slot.clone().unwrap_or_else(|| cmd.manifest.install.slot_pair.as_ref().map(|s| s[0].clone()).unwrap_or_else(|| "A".into()));
    let next_slot = compute_next_slot(&current_slot, &cmd.manifest.install.slot_pair);

    let ctx = executors::ExecutionContext {
        artifact_path: artifact_path.clone(),
        current_slot: current_slot.clone(),
        next_slot: next_slot.clone(),
        manifest: cmd.manifest.clone(),
        release_version: cmd.release_version.clone(),
        state_dir: cfg.state_dir.clone(),
    };

    executor.install(&ctx).await?;
    executor.activate(&ctx).await?;
    run_health_checks(client, &cmd.manifest.health_checks).await?;

    state.current_version = Some(cmd.release_version.clone());
    state.active_slot = Some(next_slot);
    Ok((format!("installed {}", cmd.release_version), state))
}

fn compute_next_slot(current: &str, pair: &Option<[String; 2]>) -> String {
    if let Some([a, b]) = pair {
        if current == a { b.clone() } else { a.clone() }
    } else if current == "A" { "B".into() } else { "A".into() }
}

async fn download_artifact(client: &Client, state_dir: &Path, artifact: &ArtifactRef) -> Result<PathBuf> {
    let artifact_dir = state_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)?;
    let url = Url::parse(&artifact.url)?;
    let filename = url
        .path_segments()
        .and_then(|segments| segments.last())
        .filter(|s| !s.is_empty())
        .unwrap_or("artifact.bin");
    let dest = artifact_dir.join(filename);

    match url.scheme() {
        "file" => {
            let path = url.to_file_path().map_err(|_| anyhow!("invalid file:// URL"))?;
            fs::copy(path, &dest)?;
        }
        "http" | "https" => {
            let mut headers = HeaderMap::new();
            for (k, v) in &artifact.headers {
                headers.insert(
                    HeaderName::from_bytes(k.as_bytes())?,
                    HeaderValue::from_str(v)?,
                );
            }
            let bytes = client.get(url.clone()).headers(headers).send().await?.error_for_status()?.bytes().await?;
            fs::write(&dest, &bytes)?;
        }
        other => return Err(anyhow!("unsupported artifact scheme: {other}")),
    }
    Ok(dest)
}

fn verify_sha256(path: &Path, expected: Option<&str>) -> Result<()> {
    if let Some(expected) = expected {
        let data = fs::read(path)?;
        let digest = Sha256::digest(&data);
        let actual = format!("{:x}", digest);
        if actual != expected.to_ascii_lowercase() {
            return Err(anyhow!("sha256 mismatch: expected {expected}, got {actual}"));
        }
    }
    Ok(())
}

async fn run_health_checks(client: &Client, checks: &[HealthCheck]) -> Result<()> {
    for check in checks {
        match check.kind {
            HealthCheckKind::AlwaysPass => {}
            HealthCheckKind::CommandExitZero => {
                let command = check.command.as_ref().context("missing command for command_exit_zero health check")?;
                let status = ProcessCommand::new("sh").arg("-lc").arg(command).status()?;
                if !status.success() {
                    return Err(anyhow!("health check {} failed with exit status {}", check.name, status));
                }
            }
            HealthCheckKind::HttpGet => {
                let url = check.url.as_ref().context("missing url for http_get health check")?;
                let body = client.get(url).send().await?.error_for_status()?.text().await?;
                if let Some(contains) = &check.contains {
                    if !body.contains(contains) {
                        return Err(anyhow!("health check {} body missing expected text", check.name));
                    }
                }
            }
        }
    }
    Ok(())
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

fn load_state(dir: &Path) -> Result<LocalState> {
    let path = state_path(dir);
    if !path.exists() {
        return Ok(LocalState::default());
    }
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn save_state(dir: &Path, state: &LocalState) -> Result<()> {
    let raw = serde_json::to_string_pretty(state)?;
    fs::write(state_path(dir), raw)?;
    Ok(())
}
