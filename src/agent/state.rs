use crate::{executors::RollbackAction, types::{HealthCheck, RollbackPolicy}};
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub(super) struct LocalState {
    #[serde(default)]
    pub(super) current_version: Option<String>,
    #[serde(default)]
    pub(super) active_slot: Option<String>,
    #[serde(default)]
    pub(super) pending_boot: Option<PendingBootState>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) enum PendingBootPhase {
    Forward,
    Rollback,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct PendingBootState {
    pub(super) phase: PendingBootPhase,
    pub(super) command_id: String,
    pub(super) deployment_id: String,
    pub(super) release_id: String,
    pub(super) release_version: String,
    pub(super) expected_system_path: Option<String>,
    pub(super) expected_hostname: Option<String>,
    pub(super) expected_active_slot: Option<String>,
    pub(super) expected_root_device: Option<String>,
    pub(super) next_active_slot: Option<String>,
    pub(super) previous_system_path: Option<String>,
    pub(super) previous_root_device: Option<String>,
    pub(super) previous_hostname: Option<String>,
    pub(super) previous_version: Option<String>,
    pub(super) previous_active_slot: Option<String>,
    pub(super) rollback_action: Option<RollbackAction>,
    pub(super) health_checks: Vec<HealthCheck>,
    pub(super) rollback: RollbackPolicy,
    pub(super) deadline: DateTime<Utc>,
}

pub(super) fn load_state(dir: &Path) -> Result<LocalState> {
    let path = state_path(dir);
    if !path.exists() {
        return Ok(LocalState::default());
    }
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

pub(super) fn save_state(dir: &Path, state: &LocalState) -> Result<()> {
    let raw = serde_json::to_string_pretty(state)?;
    fs::write(state_path(dir), raw)?;
    Ok(())
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}
