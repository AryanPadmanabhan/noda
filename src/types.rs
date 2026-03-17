use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub target_type: String,
    pub artifact: ArtifactRef,
    pub install: InstallConfig,
    pub activation: ActivationConfig,
    pub rollback: RollbackConfig,
    #[serde(default)]
    pub health_checks: Vec<HealthCheck>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub url: String,
    pub sha256: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallConfig {
    #[serde(default = "default_install_type")]
    pub install_type: String,
    #[serde(default = "default_executor")]
    pub executor: String,
    #[serde(default)]
    pub slot_pair: Option<[String; 2]>,
    #[serde(default)]
    pub install_command: Option<String>,
    #[serde(default)]
    pub install_args: Vec<String>,
    #[serde(default)]
    pub nix_generation: Option<NixGenerationConfig>,
}

fn default_install_type() -> String {
    "ab-image".into()
}
fn default_executor() -> String {
    "noop".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NixGenerationConfig {
    #[serde(default)]
    pub flake: Option<String>,
    #[serde(default)]
    pub flake_attr: Option<String>,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub copy_from: Option<String>,
    #[serde(default)]
    pub store_path: Option<String>,
    #[serde(default)]
    pub copy_command: Option<String>,
    #[serde(default)]
    pub build_command: Option<String>,
    #[serde(default)]
    pub boot_command: Option<String>,
    #[serde(default)]
    pub reboot_command: Option<String>,
    #[serde(default)]
    pub expected_system_path: Option<String>,
    #[serde(default)]
    pub expected_hostname: Option<String>,
    #[serde(default)]
    pub validation_timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationConfig {
    #[serde(default = "default_activation_type")]
    pub activation_type: String,
    #[serde(default)]
    pub bootloader: Option<String>,
    #[serde(default)]
    pub activate_command: Option<String>,
}
fn default_activation_type() -> String {
    "bootloader-switch".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackConfig {
    #[serde(default = "default_true")]
    pub automatic: bool,
    #[serde(default = "default_true")]
    pub on_boot_failure: bool,
    #[serde(default = "default_true")]
    pub on_health_failure: bool,
    #[serde(default)]
    pub rollback_command: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub candidate_timeout_seconds: u64,
}
fn default_true() -> bool { true }
fn default_timeout_secs() -> u64 { 900 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    pub name: String,
    pub kind: HealthCheckKind,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub contains: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthCheckKind {
    AlwaysPass,
    CommandExitZero,
    HttpGet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateReleaseRequest {
    pub version: String,
    pub manifest: ReleaseManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseRecord {
    pub id: String,
    pub version: String,
    pub target_type: String,
    pub manifest: ReleaseManifest,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Selector {
    pub target_type: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub mission_states: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutStrategy {
    #[serde(default)]
    pub canary: usize,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_max_parallel")]
    pub max_parallel: usize,
    #[serde(default = "default_failure_rate")]
    pub max_failure_rate: f64,
    #[serde(default)]
    pub require_idle: bool,
}
fn default_batch_size() -> usize { 10 }
fn default_max_parallel() -> usize { 5 }
fn default_failure_rate() -> f64 { 0.10 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDeploymentRequest {
    pub release_id: String,
    pub rollout_name: String,
    pub selector: Selector,
    pub strategy: RolloutStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentRecord {
    pub id: String,
    pub release_id: String,
    pub rollout_name: String,
    pub status: String,
    pub selector: Selector,
    pub strategy: RolloutStrategy,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentTargetRecord {
    pub deployment_id: String,
    pub asset_id: String,
    pub state: String,
    pub last_error: Option<String>,
    pub current_command_id: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCheckinRequest {
    pub asset_id: String,
    pub asset_type: String,
    pub mission_state: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub current_version: Option<String>,
    pub active_slot: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRecord {
    pub asset_id: String,
    pub asset_type: String,
    pub mission_state: String,
    pub current_version: Option<String>,
    pub desired_version: Option<String>,
    pub active_slot: Option<String>,
    pub status: String,
    pub last_seen: DateTime<Utc>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPollRequest {
    pub asset_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPollResponse {
    pub commands: Vec<CommandRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    pub id: String,
    pub deployment_id: String,
    pub release_id: String,
    pub asset_id: String,
    pub command_type: String,
    pub status: String,
    pub manifest: ReleaseManifest,
    pub release_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResultRequest {
    pub command_id: String,
    pub asset_id: String,
    pub success: bool,
    pub message: String,
    pub active_slot: Option<String>,
    pub booted_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauseDeploymentRequest {
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
    pub message: String,
}
