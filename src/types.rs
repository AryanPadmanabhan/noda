use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub target_type: String,
    pub executor: ExecutorSpec,
    #[serde(default)]
    pub validation: ValidationSpec,
    #[serde(default)]
    pub rollback: RollbackPolicy,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutorSpec {
    Noop,
    Scripted(ScriptedExecutorSpec),
    GrubAb(GrubAbExecutorSpec),
    NixGeneration(NixGenerationExecutorSpec),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptedExecutorSpec {
    pub artifact: ArtifactSource,
    pub install_command: String,
    #[serde(default)]
    pub activate_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrubAbExecutorSpec {
    pub artifact: ArtifactSource,
    #[serde(default)]
    pub slot_pair: Option<[String; 2]>,
    #[serde(default)]
    pub activate_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NixGenerationExecutorSpec {
    pub source: NixGenerationSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NixGenerationSource {
    BuildFlake {
        flake: String,
        flake_attr: String,
    },
    CopyFromStore {
        copy_from: String,
        store_path: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSource {
    pub url: String,
    pub sha256: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ValidationSpec {
    #[serde(default)]
    pub expected_hostname: Option<String>,
    #[serde(default)]
    pub expected_system_path: Option<String>,
    #[serde(default = "default_validation_timeout_secs")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub health_checks: Vec<HealthCheck>,
}

fn default_validation_timeout_secs() -> u64 {
    900
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackPolicy {
    #[serde(default = "default_true")]
    pub automatic: bool,
    #[serde(default = "default_true")]
    pub on_boot_failure: bool,
    #[serde(default = "default_true")]
    pub on_validation_failure: bool,
    #[serde(default = "default_timeout_secs")]
    pub candidate_timeout_seconds: u64,
}

impl Default for RollbackPolicy {
    fn default() -> Self {
        Self {
            automatic: true,
            on_boot_failure: true,
            on_validation_failure: true,
            candidate_timeout_seconds: default_timeout_secs(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_timeout_secs() -> u64 {
    900
}

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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

fn default_batch_size() -> usize {
    10
}

fn default_max_parallel() -> usize {
    5
}

fn default_failure_rate() -> f64 {
    0.10
}

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
