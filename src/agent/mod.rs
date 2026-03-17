use crate::{executors, types::*};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client,
};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::Duration,
};
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct LocalState {
    #[serde(default)]
    current_version: Option<String>,
    #[serde(default)]
    active_slot: Option<String>,
    #[serde(default)]
    pending_boot: Option<PendingBootState>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PendingBootState {
    command_id: String,
    deployment_id: String,
    release_id: String,
    release_version: String,
    expected_system_path: Option<String>,
    expected_hostname: Option<String>,
    next_active_slot: Option<String>,
    health_checks: Vec<HealthCheck>,
    deadline: DateTime<Utc>,
}

enum CommandExecution {
    Completed {
        message: String,
        state: LocalState,
    },
    Deferred {
        state: LocalState,
    },
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
                Ok(CommandExecution::Completed { message, state: new_state }) => {
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

async fn resume_pending_boot(client: &Client, cfg: &AgentConfig, state: &mut LocalState) -> Result<()> {
    let Some(pending) = state.pending_boot.clone() else {
        return Ok(());
    };

    match validate_pending_boot(client, &pending).await {
        Ok(()) => {
            info!(
                asset_id = %cfg.asset_id,
                command_id = %pending.command_id,
                release = %pending.release_version,
                "post-boot validation succeeded"
            );
            state.current_version = Some(pending.release_version.clone());
            state.active_slot = pending.next_active_slot.clone();
            state.pending_boot = None;
            report_result(
                client,
                cfg,
                AgentResultRequest {
                    command_id: pending.command_id,
                    asset_id: cfg.asset_id.clone(),
                    success: true,
                    message: format!("validated {} after reboot", pending.release_version),
                    active_slot: state.active_slot.clone(),
                    booted_version: state.current_version.clone(),
                },
            )
            .await?;
        }
        Err(err) if Utc::now() < pending.deadline => {
            warn!(
                asset_id = %cfg.asset_id,
                command_id = %pending.command_id,
                error = %err,
                "post-boot validation still pending"
            );
        }
        Err(err) => {
            error!(
                asset_id = %cfg.asset_id,
                command_id = %pending.command_id,
                error = %err,
                "post-boot validation timed out"
            );
            state.pending_boot = None;
            report_result(
                client,
                cfg,
                AgentResultRequest {
                    command_id: pending.command_id,
                    asset_id: cfg.asset_id.clone(),
                    success: false,
                    message: format!("post-boot validation failed: {err}"),
                    active_slot: state.active_slot.clone(),
                    booted_version: state.current_version.clone(),
                },
            )
            .await?;
        }
    }

    Ok(())
}

async fn validate_pending_boot(client: &Client, pending: &PendingBootState) -> Result<()> {
    if let Some(expected_system_path) = &pending.expected_system_path {
        let actual = current_system_path()?;
        let expected = normalize_path(expected_system_path)?;
        if actual != expected {
            return Err(anyhow!(
                "current system mismatch: expected {}, got {}",
                expected,
                actual
            ));
        }
    }

    if let Some(expected_hostname) = &pending.expected_hostname {
        let actual = current_hostname()?;
        if actual != *expected_hostname {
            return Err(anyhow!(
                "hostname mismatch: expected {}, got {}",
                expected_hostname,
                actual
            ));
        }
    }

    run_health_checks(client, &pending.health_checks).await?;
    Ok(())
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
        .json(&AgentPollRequest {
            asset_id: cfg.asset_id.clone(),
        })
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

async fn execute_command(client: &Client, cfg: &AgentConfig, cmd: &CommandRecord) -> Result<CommandExecution> {
    info!(
        asset_id = %cfg.asset_id,
        command_id = %cmd.id,
        release = %cmd.release_version,
        "executing command"
    );
    let artifact_path = if uses_nix_copy_artifact(&cmd.manifest) {
        PathBuf::from(cmd.manifest.artifact.url.clone())
    } else {
        let path = download_artifact(client, &cfg.state_dir, &cmd.manifest.artifact).await?;
        verify_sha256(&path, cmd.manifest.artifact.sha256.as_deref())?;
        path
    };

    let mut state = load_state(&cfg.state_dir)?;
    let executor = executors::build(&cmd.manifest.install.executor);
    let current_slot = state.active_slot.clone().unwrap_or_else(|| {
        cmd.manifest
            .install
            .slot_pair
            .as_ref()
            .map(|s| s[0].clone())
            .unwrap_or_else(|| "A".into())
    });
    let next_slot = compute_next_slot(&current_slot, &cmd.manifest.install.slot_pair);

    let ctx = executors::ExecutionContext {
        command_id: cmd.id.clone(),
        artifact_path: artifact_path.clone(),
        current_slot: current_slot.clone(),
        next_slot: next_slot.clone(),
        manifest: cmd.manifest.clone(),
        release_version: cmd.release_version.clone(),
        state_dir: cfg.state_dir.clone(),
    };

    executor.install(&ctx).await?;
    let activation = executor.activate(&ctx).await?;
    match activation {
        executors::ActivationOutcome::Complete => {
            run_health_checks(client, &cmd.manifest.health_checks).await?;
            state.current_version = Some(cmd.release_version.clone());
            state.active_slot = Some(next_slot);
            Ok(CommandExecution::Completed {
                message: format!("installed {}", cmd.release_version),
                state,
            })
        }
        executors::ActivationOutcome::AwaitReboot(pending) => {
            state.pending_boot = Some(PendingBootState {
                command_id: cmd.id.clone(),
                deployment_id: cmd.deployment_id.clone(),
                release_id: cmd.release_id.clone(),
                release_version: cmd.release_version.clone(),
                expected_system_path: pending.expected_system_path,
                expected_hostname: pending.expected_hostname,
                next_active_slot: Some(next_slot),
                health_checks: cmd.manifest.health_checks.clone(),
                deadline: Utc::now()
                    + ChronoDuration::seconds(i64::try_from(pending.validation_timeout_seconds).unwrap_or(900)),
            });
            Ok(CommandExecution::Deferred { state })
        }
    }
}

fn uses_nix_copy_artifact(manifest: &ReleaseManifest) -> bool {
    manifest
        .install
        .nix_generation
        .as_ref()
        .and_then(|cfg| cfg.copy_from.as_ref().zip(cfg.store_path.as_ref()))
        .is_some()
}

fn compute_next_slot(current: &str, pair: &Option<[String; 2]>) -> String {
    if let Some([a, b]) = pair {
        if current == a {
            b.clone()
        } else {
            a.clone()
        }
    } else if current == "A" {
        "B".into()
    } else {
        "A".into()
    }
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
            let path = url
                .to_file_path()
                .map_err(|_| anyhow!("invalid file:// URL"))?;
            if path.is_dir() {
                if dest.exists() {
                    fs::remove_dir_all(&dest)?;
                }
                copy_dir_all(&path, &dest)?;
            } else {
                fs::copy(path, &dest)?;
            }
        }
        "http" | "https" => {
            let mut headers = HeaderMap::new();
            for (k, v) in &artifact.headers {
                headers.insert(
                    HeaderName::from_bytes(k.as_bytes())?,
                    HeaderValue::from_str(v)?,
                );
            }
            let bytes = client
                .get(url.clone())
                .headers(headers)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?;
            fs::write(&dest, &bytes)?;
        }
        other => return Err(anyhow!("unsupported artifact scheme: {other}")),
    }
    Ok(dest)
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn verify_sha256(path: &Path, expected: Option<&str>) -> Result<()> {
    if path.is_dir() {
        return Ok(());
    }
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
                let command = check
                    .command
                    .as_ref()
                    .context("missing command for command_exit_zero health check")?;
                let status = ProcessCommand::new("sh").arg("-lc").arg(command).status()?;
                if !status.success() {
                    return Err(anyhow!(
                        "health check {} failed with exit status {}",
                        check.name,
                        status
                    ));
                }
            }
            HealthCheckKind::HttpGet => {
                let url = check
                    .url
                    .as_ref()
                    .context("missing url for http_get health check")?;
                let body = client.get(url).send().await?.error_for_status()?.text().await?;
                if let Some(contains) = &check.contains {
                    if !body.contains(contains) {
                        return Err(anyhow!(
                            "health check {} body missing expected text",
                            check.name
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn current_system_path() -> Result<String> {
    let link = env::var("DEPLOY_INTENT_CURRENT_SYSTEM_LINK").unwrap_or_else(|_| "/run/current-system".into());
    normalize_path(link)
}

fn current_hostname() -> Result<String> {
    if let Ok(path) = env::var("DEPLOY_INTENT_HOSTNAME_FILE") {
        return Ok(fs::read_to_string(path)?.trim().to_string());
    }
    let raw = fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| fs::read_to_string("/etc/hostname"))?;
    Ok(raw.trim().to_string())
}

fn normalize_path(path: impl AsRef<Path>) -> Result<String> {
    Ok(fs::canonicalize(path)?.display().to_string())
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
