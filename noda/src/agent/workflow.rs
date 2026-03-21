use super::{
    report_result,
    state::{load_state, LocalState, PendingBootPhase, PendingBootState},
    validation::{
        copy_dir_all, current_hostname, current_root_device, current_system_path, run_health_checks,
        validate_pending_boot, verify_sha256,
    },
    AgentConfig,
};
use crate::{executors, types::*};
use anyhow::{anyhow, Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::{error, info, warn};
use url::Url;

pub(super) enum CommandExecution {
    Completed { message: String, state: LocalState },
    Deferred { state: LocalState },
}

pub(super) async fn resume_pending_boot(
    client: &Client,
    cfg: &AgentConfig,
    state: &mut LocalState,
) -> Result<()> {
    let Some(pending) = state.pending_boot.clone() else {
        return Ok(());
    };

    match validate_pending_boot(client, &pending).await {
        Ok(()) => match pending.phase {
            PendingBootPhase::Forward => {
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
            PendingBootPhase::Rollback => {
                info!(
                    asset_id = %cfg.asset_id,
                    command_id = %pending.command_id,
                    release = %pending.release_version,
                    "rollback validation succeeded"
                );
                state.current_version = pending.previous_version.clone();
                state.active_slot = pending.previous_active_slot.clone();
                state.pending_boot = None;
                report_result(
                    client,
                    cfg,
                    AgentResultRequest {
                        command_id: pending.command_id,
                        asset_id: cfg.asset_id.clone(),
                        success: false,
                        message: format!(
                            "post-boot validation failed for {}; rollback succeeded",
                            pending.release_version
                        ),
                        active_slot: state.active_slot.clone(),
                        booted_version: state.current_version.clone(),
                    },
                )
                .await?;
            }
        },
        Err(err) if Utc::now() < pending.deadline => {
            warn!(
                asset_id = %cfg.asset_id,
                command_id = %pending.command_id,
                error = %err,
                "post-boot validation still pending"
            );
        }
        Err(err) => match pending.phase {
            PendingBootPhase::Forward if should_attempt_rollback(&pending) => {
                error!(
                    asset_id = %cfg.asset_id,
                    command_id = %pending.command_id,
                    error = %err,
                    "post-boot validation failed; starting rollback"
                );
                let rollback_action = pending
                    .rollback_action
                    .clone()
                    .context("missing rollback action for rollback")?;
                perform_rollback(&rollback_action)?;
                state.pending_boot = Some(PendingBootState {
                    phase: PendingBootPhase::Rollback,
                    command_id: pending.command_id,
                    deployment_id: pending.deployment_id,
                    release_id: pending.release_id,
                    release_version: pending.release_version,
                    expected_system_path: pending.previous_system_path.clone(),
                    expected_hostname: pending.previous_hostname,
                    expected_active_slot: pending.previous_active_slot.clone(),
                    expected_root_device: pending.previous_root_device.clone(),
                    next_active_slot: pending.previous_active_slot.clone(),
                    previous_system_path: pending.previous_system_path,
                    previous_root_device: pending.previous_root_device,
                    previous_hostname: None,
                    previous_version: pending.previous_version,
                    previous_active_slot: pending.previous_active_slot,
                    rollback_action: None,
                    health_checks: pending.health_checks,
                    rollback: pending.rollback.clone(),
                    deadline: Utc::now()
                        + ChronoDuration::seconds(
                            i64::try_from(pending.rollback.candidate_timeout_seconds)
                                .unwrap_or(900),
                        ),
                });
            }
            PendingBootPhase::Forward => {
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
            PendingBootPhase::Rollback => {
                error!(
                    asset_id = %cfg.asset_id,
                    command_id = %pending.command_id,
                    error = %err,
                    "rollback validation failed"
                );
                state.pending_boot = None;
                report_result(
                    client,
                    cfg,
                    AgentResultRequest {
                        command_id: pending.command_id,
                        asset_id: cfg.asset_id.clone(),
                        success: false,
                        message: format!("post-boot validation failed and rollback failed: {err}"),
                        active_slot: state.active_slot.clone(),
                        booted_version: state.current_version.clone(),
                    },
                )
                .await?;
            }
        },
    }

    Ok(())
}

pub(super) async fn execute_command(
    client: &Client,
    cfg: &AgentConfig,
    cmd: &CommandRecord,
) -> Result<CommandExecution> {
    info!(
        asset_id = %cfg.asset_id,
        command_id = %cmd.id,
        release = %cmd.release_version,
        "executing command"
    );
    let artifact_path = if let Some(artifact) = artifact_source(&cmd.manifest.executor) {
        let path = download_artifact(client, &cfg.state_dir, artifact).await?;
        verify_sha256(&path, artifact.sha256.as_deref())?;
        Some(path)
    } else {
        None
    };

    let mut state = load_state(&cfg.state_dir)?;
    let executor = executors::build(&cmd.manifest.executor);
    let slot_pair = slot_pair(&cmd.manifest.executor);
    let current_slot = executors::detect_current_slot(&cmd.manifest.executor)?
        .or_else(|| state.active_slot.clone())
        .unwrap_or_else(|| {
            slot_pair
                .as_ref()
                .map(|slots| slots[0].clone())
                .unwrap_or_else(|| "A".into())
        });
    let next_slot = compute_next_slot(&current_slot, slot_pair.as_ref());
    let previous_system_path = if matches!(cmd.manifest.executor, ExecutorSpec::NixGeneration(_)) {
        Some(current_system_path()?)
    } else {
        None
    };
    let previous_hostname = if requires_reboot(&cmd.manifest.executor) {
        Some(current_hostname()?)
    } else {
        None
    };
    let previous_active_slot = Some(current_slot.clone()).or_else(|| state.active_slot.clone());
    let previous_root_device = match &cmd.manifest.executor {
        ExecutorSpec::GrubAb(spec) if spec.slots.is_some() => Some(current_root_device()?),
        ExecutorSpec::GrubAb(_) | ExecutorSpec::Noop | ExecutorSpec::Scripted(_) | ExecutorSpec::NixGeneration(_) => None,
    };
    let previous_version = state.current_version.clone();
    let rollback_action = executors::rollback_action(
        &cmd.manifest.executor,
        &current_slot,
        previous_system_path.clone(),
    )?;

    let ctx = executors::ExecutionContext {
        command_id: cmd.id.clone(),
        artifact_path,
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
            run_health_checks(client, &cmd.manifest.validation.health_checks).await?;
            state.current_version = Some(cmd.release_version.clone());
            state.active_slot = Some(next_slot);
            Ok(CommandExecution::Completed {
                message: format!("installed {}", cmd.release_version),
                state,
            })
        }
        executors::ActivationOutcome::AwaitReboot(pending) => {
            state.pending_boot = Some(PendingBootState {
                phase: PendingBootPhase::Forward,
                command_id: cmd.id.clone(),
                deployment_id: cmd.deployment_id.clone(),
                release_id: cmd.release_id.clone(),
                release_version: cmd.release_version.clone(),
                expected_system_path: pending
                    .expected_system_path
                    .or_else(|| cmd.manifest.validation.expected_system_path.clone()),
                expected_hostname: cmd.manifest.validation.expected_hostname.clone(),
                expected_active_slot: pending.expected_active_slot.or_else(|| {
                    matches!(cmd.manifest.executor, ExecutorSpec::GrubAb(_))
                        .then(|| next_slot.clone())
                }),
                expected_root_device: pending.expected_root_device,
                next_active_slot: Some(next_slot),
                previous_system_path,
                previous_root_device,
                previous_hostname,
                previous_version,
                previous_active_slot,
                rollback_action,
                health_checks: cmd.manifest.validation.health_checks.clone(),
                rollback: cmd.manifest.rollback.clone(),
                deadline: Utc::now()
                    + ChronoDuration::seconds(
                        i64::try_from(cmd.manifest.validation.timeout_seconds).unwrap_or(900),
                    ),
            });
            Ok(CommandExecution::Deferred { state })
        }
    }
}

fn should_attempt_rollback(pending: &PendingBootState) -> bool {
    pending.rollback.automatic && pending.rollback.on_validation_failure && pending.rollback_action.is_some()
}

fn artifact_source(executor: &ExecutorSpec) -> Option<&ArtifactSource> {
    match executor {
        ExecutorSpec::Scripted(spec) => Some(&spec.artifact),
        ExecutorSpec::GrubAb(spec) => Some(&spec.artifact),
        ExecutorSpec::Noop | ExecutorSpec::NixGeneration(_) => None,
    }
}

fn slot_pair(executor: &ExecutorSpec) -> Option<[String; 2]> {
    match executor {
        ExecutorSpec::GrubAb(spec) => spec
            .slot_pair
            .clone()
            .or_else(|| spec.slots.as_ref().map(|slots| [slots[0].name.clone(), slots[1].name.clone()])),
        ExecutorSpec::Noop | ExecutorSpec::Scripted(_) | ExecutorSpec::NixGeneration(_) => None,
    }
}

fn compute_next_slot(current: &str, pair: Option<&[String; 2]>) -> String {
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

fn requires_reboot(executor: &ExecutorSpec) -> bool {
    match executor {
        ExecutorSpec::NixGeneration(_) => true,
        ExecutorSpec::GrubAb(spec) => spec.slots.is_some(),
        ExecutorSpec::Noop | ExecutorSpec::Scripted(_) => false,
    }
}

fn perform_rollback(action: &executors::RollbackAction) -> Result<()> {
    match action {
        executors::RollbackAction::NixGeneration {
            previous_system_path,
        } => executors::rollback_nix_generation(previous_system_path),
        executors::RollbackAction::GrubAb {
            authority_device,
            mountpoint,
            grubenv_relpath,
            previous_grub_menu_entry,
        } => executors::rollback_grub_ab(
            authority_device,
            mountpoint,
            grubenv_relpath,
            previous_grub_menu_entry,
        ),
    }
}

async fn download_artifact(
    client: &Client,
    state_dir: &Path,
    artifact: &ArtifactSource,
) -> Result<PathBuf> {
    let artifact_dir = state_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)?;
    let url = Url::parse(&artifact.url)?;
    let filename = url
        .path_segments()
        .and_then(|segments| segments.last())
        .filter(|segment| !segment.is_empty())
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
            for (key, value) in &artifact.headers {
                headers.insert(
                    HeaderName::from_bytes(key.as_bytes())?,
                    HeaderValue::from_str(value)?,
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
