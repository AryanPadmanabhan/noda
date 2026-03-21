use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use url::Url;

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
    pub slots: Option<[GrubAbSlot; 2]>,
    #[serde(default)]
    pub boot_control: Option<GrubAbBootControl>,
    #[serde(default)]
    pub compression: GrubAbCompression,
    #[serde(default)]
    pub activate_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrubAbSlot {
    pub name: String,
    pub device: String,
    pub grub_menu_entry: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrubAbBootControl {
    pub authority_device: String,
    pub mountpoint: String,
    #[serde(default = "default_grubenv_relpath")]
    pub grubenv_relpath: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrubAbCompression {
    None,
    Zstd,
    #[default]
    Auto,
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

impl ValidationSpec {
    pub fn validate(&self) -> Result<()> {
        if self.timeout_seconds == 0 {
            return Err(anyhow!(
                "validation.timeout_seconds must be greater than zero"
            ));
        }

        for check in &self.health_checks {
            match check.kind {
                HealthCheckKind::AlwaysPass => {}
                HealthCheckKind::CommandExitZero => {
                    let command = check
                        .command
                        .as_ref()
                        .context("command_exit_zero health check requires command")?;
                    if command.trim().is_empty() {
                        return Err(anyhow!(
                            "command_exit_zero health check command must not be empty"
                        ));
                    }
                }
                HealthCheckKind::HttpGet => {
                    let url = check
                        .url
                        .as_ref()
                        .context("http_get health check requires url")?;
                    Url::parse(url).with_context(|| format!("invalid health check url: {url}"))?;
                }
            }
        }

        Ok(())
    }
}

fn default_validation_timeout_secs() -> u64 {
    900
}

fn default_grubenv_relpath() -> String {
    "/boot/grub/grubenv".into()
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

impl CreateReleaseRequest {
    pub fn validate(&self) -> Result<()> {
        if self.version.trim().is_empty() {
            return Err(anyhow!("release version must not be empty"));
        }
        self.manifest.validate()
    }
}

impl ReleaseManifest {
    pub fn validate(&self) -> Result<()> {
        if self.target_type.trim().is_empty() {
            return Err(anyhow!("manifest target_type must not be empty"));
        }

        self.validation.validate()?;

        match &self.executor {
            ExecutorSpec::Noop => {}
            ExecutorSpec::Scripted(spec) => {
                validate_artifact_source(&spec.artifact)?;
                if spec.install_command.trim().is_empty() {
                    return Err(anyhow!("scripted.install_command must not be empty"));
                }
                if let Some(command) = &spec.activate_command {
                    if command.trim().is_empty() {
                        return Err(anyhow!(
                            "scripted.activate_command must not be empty when provided"
                        ));
                    }
                }
            }
            ExecutorSpec::GrubAb(spec) => {
                validate_artifact_source(&spec.artifact)?;
                if let Some([left, right]) = &spec.slot_pair {
                    if left.trim().is_empty() || right.trim().is_empty() {
                        return Err(anyhow!("grub_ab.slot_pair entries must not be empty"));
                    }
                    if left == right {
                        return Err(anyhow!("grub_ab.slot_pair entries must be distinct"));
                    }
                }
                if let Some(slots) = &spec.slots {
                    for slot in slots {
                        if slot.name.trim().is_empty() {
                            return Err(anyhow!("grub_ab.slots names must not be empty"));
                        }
                        if slot.device.trim().is_empty() {
                            return Err(anyhow!("grub_ab.slots devices must not be empty"));
                        }
                        if slot.grub_menu_entry.trim().is_empty() {
                            return Err(anyhow!(
                                "grub_ab.slots grub_menu_entry values must not be empty"
                            ));
                        }
                    }
                    if slots[0].name == slots[1].name {
                        return Err(anyhow!("grub_ab.slots names must be distinct"));
                    }
                    if slots[0].device == slots[1].device {
                        return Err(anyhow!("grub_ab.slots devices must be distinct"));
                    }
                    if let Some([left, right]) = &spec.slot_pair {
                        if slots[0].name != *left || slots[1].name != *right {
                            return Err(anyhow!(
                                "grub_ab.slot_pair must match grub_ab.slots order when both are provided"
                            ));
                        }
                    }
                    if spec.boot_control.is_none() {
                        return Err(anyhow!(
                            "grub_ab.boot_control is required when grub_ab.slots is provided"
                        ));
                    }
                }
                if let Some(boot_control) = &spec.boot_control {
                    if boot_control.authority_device.trim().is_empty() {
                        return Err(anyhow!(
                            "grub_ab.boot_control.authority_device must not be empty"
                        ));
                    }
                    if boot_control.mountpoint.trim().is_empty() {
                        return Err(anyhow!(
                            "grub_ab.boot_control.mountpoint must not be empty"
                        ));
                    }
                    if boot_control.grubenv_relpath.trim().is_empty() {
                        return Err(anyhow!(
                            "grub_ab.boot_control.grubenv_relpath must not be empty"
                        ));
                    }
                }
                if let Some(command) = &spec.activate_command {
                    if command.trim().is_empty() {
                        return Err(anyhow!(
                            "grub_ab.activate_command must not be empty when provided"
                        ));
                    }
                }
            }
            ExecutorSpec::NixGeneration(spec) => match &spec.source {
                NixGenerationSource::BuildFlake { flake, flake_attr } => {
                    if flake.trim().is_empty() {
                        return Err(anyhow!(
                            "nix_generation.build_flake.flake must not be empty"
                        ));
                    }
                    if flake_attr.trim().is_empty() {
                        return Err(anyhow!(
                            "nix_generation.build_flake.flake_attr must not be empty"
                        ));
                    }
                }
                NixGenerationSource::CopyFromStore {
                    copy_from,
                    store_path,
                } => {
                    if copy_from.trim().is_empty() {
                        return Err(anyhow!(
                            "nix_generation.copy_from_store.copy_from must not be empty"
                        ));
                    }
                    if store_path.trim().is_empty() {
                        return Err(anyhow!(
                            "nix_generation.copy_from_store.store_path must not be empty"
                        ));
                    }
                    if !store_path.starts_with("/nix/store/") {
                        return Err(anyhow!(
                            "nix_generation.copy_from_store.store_path must be a /nix/store path"
                        ));
                    }
                }
            },
        }

        Ok(())
    }
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
    pub status: DeploymentStatus,
    pub selector: Selector,
    pub strategy: RolloutStrategy,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentTargetRecord {
    pub deployment_id: String,
    pub asset_id: String,
    pub state: DeploymentTargetState,
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
    pub status: Option<AssetStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRecord {
    pub asset_id: String,
    pub asset_type: String,
    pub mission_state: String,
    pub current_version: Option<String>,
    pub desired_version: Option<String>,
    pub active_slot: Option<String>,
    pub status: AssetStatus,
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
    pub status: CommandStatus,
    pub manifest: ReleaseManifest,
    pub release_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStatus {
    Active,
    Paused,
    Aborted,
}

impl DeploymentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Aborted => "aborted",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "aborted" => Ok(Self::Aborted),
            _ => Err(anyhow!("unknown deployment status: {raw}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentTargetState {
    Pending,
    Issued,
    Retry,
    Succeeded,
    Failed,
    RolledBack,
}

impl DeploymentTargetState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Issued => "issued",
            Self::Retry => "retry",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::RolledBack => "rolled_back",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "issued" => Ok(Self::Issued),
            "retry" => Ok(Self::Retry),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "rolled_back" => Ok(Self::RolledBack),
            _ => Err(anyhow!("unknown deployment target state: {raw}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl CommandStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    Online,
    Offline,
}

impl AssetStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "online" => Ok(Self::Online),
            "offline" => Ok(Self::Offline),
            _ => Err(anyhow!("unknown asset status: {raw}")),
        }
    }
}

fn validate_artifact_source(artifact: &ArtifactSource) -> Result<()> {
    if artifact.url.trim().is_empty() {
        return Err(anyhow!("artifact.url must not be empty"));
    }
    Url::parse(&artifact.url).with_context(|| format!("invalid artifact url: {}", artifact.url))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_nix_copy_manifest() -> ReleaseManifest {
        serde_json::from_value(json!({
            "target_type": "edge-linux-aarch64",
            "executor": {
                "kind": "nix_generation",
                "source": {
                    "kind": "copy_from_store",
                    "copy_from": "ssh://builder@example",
                    "store_path": "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-system"
                }
            },
            "validation": {
                "timeout_seconds": 30
            }
        }))
        .expect("valid manifest")
    }

    #[test]
    fn create_release_validation_accepts_valid_nix_copy_manifest() {
        let req = CreateReleaseRequest {
            version: "1.2.3".into(),
            manifest: valid_nix_copy_manifest(),
        };

        req.validate().expect("request should validate");
    }

    #[test]
    fn create_release_validation_rejects_invalid_store_path() {
        let mut manifest = valid_nix_copy_manifest();
        let ExecutorSpec::NixGeneration(spec) = &mut manifest.executor else {
            panic!("expected nix_generation executor");
        };
        spec.source = NixGenerationSource::CopyFromStore {
            copy_from: "ssh://builder@example".into(),
            store_path: "/tmp/not-a-store-path".into(),
        };

        let req = CreateReleaseRequest {
            version: "1.2.3".into(),
            manifest,
        };

        let err = req.validate().expect_err("request should fail validation");
        assert!(err.to_string().contains("/nix/store"));
    }

    #[test]
    fn validation_spec_rejects_http_check_without_url() {
        let spec = ValidationSpec {
            expected_hostname: None,
            expected_system_path: None,
            timeout_seconds: 30,
            health_checks: vec![HealthCheck {
                name: "missing-url".into(),
                kind: HealthCheckKind::HttpGet,
                command: None,
                url: None,
                contains: None,
            }],
        };

        let err = spec.validate().expect_err("validation should fail");
        assert!(err.to_string().contains("requires url"));
    }

    #[test]
    fn deployment_target_state_serializes_as_snake_case() {
        let encoded = serde_json::to_string(&DeploymentTargetState::RolledBack)
            .expect("serialize deployment target state");
        assert_eq!(encoded, "\"rolled_back\"");
    }

    #[test]
    fn create_release_validation_rejects_mismatched_grub_slot_pair() {
        let manifest: ReleaseManifest = serde_json::from_value(json!({
            "target_type": "edge-linux-x86",
            "executor": {
                "kind": "grub_ab",
                "artifact": {
                    "url": "file:///tmp/artifact.ext4",
                    "sha256": null,
                    "headers": {}
                },
                "slot_pair": ["A", "B"],
                "slots": [
                    {
                        "name": "B",
                        "device": "/dev/disk/by-partlabel/root-b",
                        "grub_menu_entry": "noda-slot-b"
                    },
                    {
                        "name": "A",
                        "device": "/dev/disk/by-partlabel/root-a",
                        "grub_menu_entry": "noda-slot-a"
                    }
                ]
            }
        }))
        .expect("valid json");

        manifest.validate().expect_err("validation should fail");
    }
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
