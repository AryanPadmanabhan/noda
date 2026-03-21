use super::state::PendingBootState;
use crate::types::{HealthCheck, HealthCheckKind};
use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path, process::Command as ProcessCommand};

pub(super) async fn validate_pending_boot(
    client: &Client,
    pending: &PendingBootState,
) -> Result<()> {
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

    if let Some(expected_root_device) = &pending.expected_root_device {
        let actual = current_root_device()?;
        let expected = normalize_device(expected_root_device)?;
        if actual != expected {
            return Err(anyhow!(
                "root device mismatch: expected {}, got {}",
                expected,
                actual
            ));
        }
    }

    run_health_checks(client, &pending.health_checks).await?;
    Ok(())
}

pub(super) async fn run_health_checks(client: &Client, checks: &[HealthCheck]) -> Result<()> {
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
                let body = client
                    .get(url)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
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

pub(super) fn verify_sha256(path: &Path, expected: Option<&str>) -> Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(expected) = expected {
        let data = fs::read(path)?;
        let digest = Sha256::digest(&data);
        let actual = format!("{:x}", digest);
        if actual != expected.to_ascii_lowercase() {
            return Err(anyhow!(
                "sha256 mismatch: expected {expected}, got {actual}"
            ));
        }
    }
    Ok(())
}

pub(super) fn current_system_path() -> Result<String> {
    let link = env::var("DEPLOY_INTENT_CURRENT_SYSTEM_LINK")
        .unwrap_or_else(|_| "/run/current-system".into());
    normalize_path(link)
}

pub(super) fn current_hostname() -> Result<String> {
    if let Ok(path) = env::var("DEPLOY_INTENT_HOSTNAME_FILE") {
        return Ok(fs::read_to_string(path)?.trim().to_string());
    }
    if let Ok(raw) = fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| fs::read_to_string("/etc/hostname"))
    {
        return Ok(raw.trim().to_string());
    }
    let output = ProcessCommand::new("hostname")
        .output()
        .context("running hostname to detect current hostname")?;
    if !output.status.success() {
        return Err(anyhow!("hostname command failed while detecting current hostname"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(super) fn current_root_device() -> Result<String> {
    if let Ok(path) = env::var("NODA_GRUB_AB_ACTIVE_DEVICE_FILE") {
        return Ok(normalize_device(fs::read_to_string(path)?.trim())?);
    }

    let output = ProcessCommand::new("findmnt")
        .args(["-n", "-o", "SOURCE", "/"])
        .output()
        .context("running findmnt to detect current root device")?;
    if !output.status.success() {
        return Err(anyhow!("findmnt failed while detecting current root device"));
    }

    let source = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if source.is_empty() {
        return Err(anyhow!("findmnt did not return a current root device"));
    }

    normalize_device(&source)
}

fn normalize_device(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    if path.exists() {
        return Ok(fs::canonicalize(path)?.display().to_string());
    }
    Ok(path.display().to_string())
}

pub(super) fn normalize_path(path: impl AsRef<Path>) -> Result<String> {
    Ok(fs::canonicalize(path)?.display().to_string())
}

pub(super) fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dest = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}
