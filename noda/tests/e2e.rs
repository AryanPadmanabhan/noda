use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tokio::time::sleep;
use url::Url;
use uuid::Uuid;

#[derive(Debug)]
struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
struct TestEnv {
    root: PathBuf,
    server_url: String,
    _server: ChildGuard,
    _agents: Vec<ChildGuard>,
    client: Client,
}

#[derive(Debug, Clone, Deserialize)]
struct ReleaseRecord {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DeploymentRecord {
    id: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AssetRecord {
    asset_id: String,
    current_version: Option<String>,
    mission_state: String,
    active_slot: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeploymentTargetRecord {
    asset_id: String,
    state: String,
    last_error: Option<String>,
}

fn bin_path() -> PathBuf {
    if let Ok(p) = env::var("CARGO_BIN_EXE_noda") {
        return PathBuf::from(p);
    }

    let exe_name = if cfg!(windows) { "noda.exe" } else { "noda" };

    // Typical integration-test layout: target/debug/deps/<test-binary>
    if let Ok(current) = env::current_exe() {
        if let Some(debug_dir) = current.parent().and_then(|p| p.parent()) {
            let candidate = debug_dir.join(exe_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // Fallback: build the main binary if Cargo didn't expose CARGO_BIN_EXE_*
    let status = Command::new("cargo")
        .args(["build", "--bin", "noda"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("failed to invoke cargo build for noda binary");
    assert!(status.success(), "cargo build --bin noda failed");

    let candidate = PathBuf::from("target").join("debug").join(exe_name);
    assert!(
        candidate.exists(),
        "noda binary not found at {}",
        candidate.display()
    );
    candidate
}

fn unique_root(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("noda-tests-{}-{}", name, Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn start_server(root: &Path) -> (ChildGuard, String) {
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    let server_url = format!("http://{bind}");
    let db_path = root.join("noda.db");

    let child = Command::new(bin_path())
        .arg("server")
        .arg("--bind")
        .arg(&bind)
        .arg("--db")
        .arg(&db_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");

    let guard = ChildGuard { child };
    let client = Client::new();
    wait_until(Duration::from_secs(15), || {
        let client = client.clone();
        let url = format!("{server_url}/healthz");
        async move {
            Some(
                client
                    .get(url)
                    .send()
                    .await
                    .ok()
                    .and_then(|r| r.error_for_status().ok())
                    .is_some(),
            )
        }
    })
    .await;

    (guard, server_url)
}

fn spawn_agent(
    server_url: &str,
    root: &Path,
    asset_id: &str,
    asset_type: &str,
    mission_state: &str,
    labels: &[&str],
) -> ChildGuard {
    spawn_agent_with_env(
        server_url,
        root,
        asset_id,
        asset_type,
        mission_state,
        labels,
        &[],
    )
}

fn spawn_agent_with_env(
    server_url: &str,
    root: &Path,
    asset_id: &str,
    asset_type: &str,
    mission_state: &str,
    labels: &[&str],
    envs: &[(&str, &str)],
) -> ChildGuard {
    let state_dir = root.join(format!("state-{asset_id}"));
    fs::create_dir_all(&state_dir).unwrap();

    let mut cmd = Command::new(bin_path());
    cmd.arg("agent")
        .arg("--server")
        .arg(server_url)
        .arg("--asset-id")
        .arg(asset_id)
        .arg("--asset-type")
        .arg(asset_type)
        .arg("--mission-state")
        .arg(mission_state)
        .arg("--poll-seconds")
        .arg("1")
        .arg("--state-dir")
        .arg(&state_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    for (key, value) in envs {
        cmd.env(key, value);
    }

    for label in labels {
        cmd.arg("--labels").arg(label);
    }

    let child = cmd.spawn().expect("spawn agent");
    ChildGuard { child }
}

async fn test_env(name: &str, agents: &[(&str, &str, &str, Vec<&str>)]) -> TestEnv {
    let root = unique_root(name);
    let (server, server_url) = start_server(&root).await;
    let client = Client::new();

    let mut spawned = Vec::new();
    for (asset_id, asset_type, mission_state, labels) in agents {
        spawned.push(spawn_agent(
            &server_url,
            &root,
            asset_id,
            asset_type,
            mission_state,
            labels,
        ));
    }

    wait_until(Duration::from_secs(20), || {
        let client = client.clone();
        let server_url = server_url.clone();
        let expected = agents.len();
        async move {
            let resp = client
                .get(format!("{server_url}/v1/assets"))
                .send()
                .await
                .ok()?;
            let assets = resp
                .error_for_status()
                .ok()?
                .json::<Vec<AssetRecord>>()
                .await
                .ok()?;
            Some(assets.len() == expected)
        }
    })
    .await;

    TestEnv {
        root,
        server_url,
        _server: server,
        _agents: spawned,
        client,
    }
}

fn make_artifact(root: &Path, name: &str, contents: &[u8]) -> (PathBuf, String) {
    let path = root.join(name);
    fs::write(&path, contents).unwrap();
    let digest = Sha256::digest(contents);
    (path, format!("{:x}", digest))
}

fn make_executable(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, body).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

#[derive(Debug)]
struct GrubAbHarness {
    slot_a: PathBuf,
    slot_b: PathBuf,
    authority_root: PathBuf,
    authority_mountpoint: PathBuf,
    active_device_file: PathBuf,
    reboot_script: PathBuf,
    grub_editenv_script: PathBuf,
    mount_script: PathBuf,
    umount_script: PathBuf,
}

fn make_grub_ab_harness(root: &Path) -> GrubAbHarness {
    let slot_a = root.join("slot-a.img");
    let slot_b = root.join("slot-b.img");
    let authority_root = root.join("authority-root-a");
    let authority_mountpoint = root.join("authority-mount");
    let grubenv = authority_root.join("boot/grub/grubenv");
    let active_device_file = root.join("active-device");

    fs::create_dir_all(grubenv.parent().unwrap()).unwrap();
    fs::create_dir_all(&authority_mountpoint).unwrap();
    fs::write(&slot_a, b"slot-a-initial").unwrap();
    fs::write(&slot_b, b"slot-b-initial").unwrap();
    fs::write(&grubenv, "saved_entry=noda-slot-a\n").unwrap();
    fs::write(&active_device_file, format!("{}\n", slot_a.display())).unwrap();
    let slot_a_canonical = fs::canonicalize(&slot_a).unwrap();

    let grub_editenv_script = make_executable(
        root,
        "fake-grub-editenv.sh",
        r#"#!/bin/sh
grubenv="$1"
cmd="$2"
assignment="$3"
[ "$cmd" = "set" ] || exit 1
printf '%s\n' "$assignment" > "$grubenv"
"#,
    );
    let mount_script = make_executable(
        root,
        "fake-mount.sh",
        &format!(
            r#"#!/bin/sh
device="$1"
mountpoint="$2"
[ "$device" = "{slot_a}" ] || [ "$device" = "{slot_a_canonical}" ] || exit 1
rm -rf "$mountpoint"
ln -s "{authority_root}" "$mountpoint"
"#,
            slot_a = slot_a.display(),
            slot_a_canonical = slot_a_canonical.display(),
            authority_root = authority_root.display(),
        ),
    );
    let umount_script = make_executable(
        root,
        "fake-umount.sh",
        r#"#!/bin/sh
mountpoint="$1"
rm -rf "$mountpoint"
mkdir -p "$mountpoint"
"#,
    );
    let reboot_script = make_executable(
        root,
        "fake-reboot.sh",
        &format!(
            r#"#!/bin/sh
grep -q 'saved_entry=noda-slot-b' "{grubenv}" && printf '%s\n' "{slot_b}" > "{active_device}" || printf '%s\n' "{slot_a}" > "{active_device}"
exit 0
"#,
            grubenv = grubenv.display(),
            slot_a = slot_a.display(),
            slot_b = slot_b.display(),
            active_device = active_device_file.display(),
        ),
    );

    GrubAbHarness {
        slot_a,
        slot_b,
        authority_root,
        authority_mountpoint,
        active_device_file,
        reboot_script,
        grub_editenv_script,
        mount_script,
        umount_script,
    }
}

async fn create_release(
    env: &TestEnv,
    version: &str,
    target_type: &str,
    artifact_url: String,
    sha256: Option<String>,
    executor: &str,
    health_checks: Vec<Value>,
) -> ReleaseRecord {
    let executor_kind = match executor {
        "grub-ab" => "grub_ab",
        other => other,
    };
    let body = json!({
        "version": version,
        "manifest": {
            "target_type": target_type,
            "executor": {
                "kind": executor_kind,
                "artifact": {
                    "url": artifact_url,
                    "sha256": sha256,
                    "headers": {}
                },
                "slot_pair": ["A", "B"]
            },
            "validation": {
                "health_checks": health_checks
            },
            "rollback": {
                "automatic": true,
                "on_boot_failure": true,
                "on_validation_failure": true,
                "candidate_timeout_seconds": 900
            },
            "labels": {"track": "test"}
        }
    });

    env.client
        .post(format!("{}/v1/releases", env.server_url))
        .json(&body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<ReleaseRecord>()
        .await
        .unwrap()
}

async fn create_release_from_manifest(
    env: &TestEnv,
    version: &str,
    manifest: Value,
) -> ReleaseRecord {
    env.client
        .post(format!("{}/v1/releases", env.server_url))
        .json(&json!({
            "version": version,
            "manifest": manifest,
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<ReleaseRecord>()
        .await
        .unwrap()
}

async fn create_grub_ab_release(
    env: &TestEnv,
    version: &str,
    artifact_url: String,
    sha256: Option<String>,
    harness: &GrubAbHarness,
    validation: Value,
) -> ReleaseRecord {
    create_release_from_manifest(
        env,
        version,
        json!({
            "target_type": "edge-linux-x86",
            "executor": {
                "kind": "grub_ab",
                "artifact": {
                    "url": artifact_url,
                    "sha256": sha256,
                    "headers": {}
                },
                "slot_pair": ["A", "B"],
                "slots": [
                    {
                        "name": "A",
                        "device": harness.slot_a,
                        "grub_menu_entry": "noda-slot-a"
                    },
                    {
                        "name": "B",
                        "device": harness.slot_b,
                        "grub_menu_entry": "noda-slot-b"
                    }
                ],
                "boot_control": {
                    "authority_device": harness.slot_a,
                    "mountpoint": harness.authority_mountpoint,
                    "grubenv_relpath": "/boot/grub/grubenv"
                },
                "compression": "none"
            },
            "validation": validation,
            "rollback": {
                "automatic": true,
                "on_boot_failure": true,
                "on_validation_failure": true,
                "candidate_timeout_seconds": 2
            },
            "labels": {"track": "test"}
        }),
    )
    .await
}

async fn create_deployment(
    env: &TestEnv,
    release_id: &str,
    target_type: &str,
    labels: Value,
    mission_states: Vec<&str>,
    canary: usize,
    max_parallel: usize,
    max_failure_rate: f64,
    require_idle: bool,
) -> DeploymentRecord {
    let body = json!({
        "release_id": release_id,
        "rollout_name": format!("rollout-{}", Uuid::new_v4()),
        "selector": {
            "target_type": target_type,
            "labels": labels,
            "mission_states": mission_states
        },
        "strategy": {
            "canary": canary,
            "batch_size": 10,
            "max_parallel": max_parallel,
            "max_failure_rate": max_failure_rate,
            "require_idle": require_idle
        }
    });

    env.client
        .post(format!("{}/v1/deployments", env.server_url))
        .json(&body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<DeploymentRecord>()
        .await
        .unwrap()
}

async fn deployment(env: &TestEnv, deployment_id: &str) -> DeploymentRecord {
    env.client
        .get(format!("{}/v1/deployments/{deployment_id}", env.server_url))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<DeploymentRecord>()
        .await
        .unwrap()
}

async fn targets(env: &TestEnv, deployment_id: &str) -> Vec<DeploymentTargetRecord> {
    env.client
        .get(format!(
            "{}/v1/deployments/{deployment_id}/targets",
            env.server_url
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Vec<DeploymentTargetRecord>>()
        .await
        .unwrap()
}

async fn assets(env: &TestEnv) -> Vec<AssetRecord> {
    env.client
        .get(format!("{}/v1/assets", env.server_url))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Vec<AssetRecord>>()
        .await
        .unwrap()
}

async fn wait_until<F, Fut>(timeout: Duration, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<bool>>,
{
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(true) = condition().await {
            return;
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("condition not met within {:?}", timeout);
}

fn always_pass_check() -> Value {
    json!({"name": "always-pass", "kind": "always_pass"})
}

fn sleep_success_check(seconds: u64) -> Value {
    json!({
        "name": format!("sleep-success-{seconds}"),
        "kind": "command_exit_zero",
        "command": format!("sleep {seconds}; exit 0")
    })
}

fn fail_check() -> Value {
    json!({
        "name": "always-fail",
        "kind": "command_exit_zero",
        "command": "exit 1"
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_agent_update_succeeds() {
    let env = test_env(
        "single-agent",
        &[("node-01", "edge-linux-x86", "idle", vec!["region=lab"])],
    )
    .await;

    let (artifact_path, sha256) = make_artifact(&env.root, "artifact.bin", b"release-v1-single");
    let release = create_release(
        &env,
        "1.0.0",
        "edge-linux-x86",
        Url::from_file_path(artifact_path).unwrap().to_string(),
        Some(sha256),
        "grub-ab",
        vec![always_pass_check()],
    )
    .await;

    let deployment = create_deployment(
        &env,
        &release.id,
        "edge-linux-x86",
        json!({"region": "lab"}),
        vec!["idle"],
        0,
        4,
        1.0,
        true,
    )
    .await;

    wait_until(Duration::from_secs(15), || {
        let env = &env;
        let deployment_id = deployment.id.clone();
        async move {
            let ts = targets(env, &deployment_id).await;
            Some(ts.len() == 1 && ts[0].state == "succeeded")
        }
    })
    .await;

    let asets = assets(&env).await;
    assert_eq!(asets.len(), 1);
    assert_eq!(asets[0].asset_id, "node-01");
    assert_eq!(asets[0].current_version.as_deref(), Some("1.0.0"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_agent_update_succeeds() {
    let mut defs = Vec::new();
    for i in 1..=10 {
        defs.push((
            format!("node-{i:02}"),
            "edge-linux-x86".to_string(),
            "idle".to_string(),
            vec!["region=lab".to_string()],
        ));
    }
    let borrowed: Vec<(&str, &str, &str, Vec<&str>)> = defs
        .iter()
        .map(|(a, t, m, labels)| {
            (
                a.as_str(),
                t.as_str(),
                m.as_str(),
                labels.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    let env = test_env("ten-agent", &borrowed).await;

    let (artifact_path, sha256) = make_artifact(&env.root, "artifact.bin", b"release-v1-ten");
    let release = create_release(
        &env,
        "2.0.0",
        "edge-linux-x86",
        Url::from_file_path(artifact_path).unwrap().to_string(),
        Some(sha256),
        "grub-ab",
        vec![always_pass_check()],
    )
    .await;

    let deployment = create_deployment(
        &env,
        &release.id,
        "edge-linux-x86",
        json!({"region": "lab"}),
        vec!["idle"],
        0,
        10,
        1.0,
        true,
    )
    .await;

    wait_until(Duration::from_secs(30), || {
        let env = &env;
        let deployment_id = deployment.id.clone();
        async move {
            let ts = targets(env, &deployment_id).await;
            Some(ts.len() == 10 && ts.iter().all(|t| t.state == "succeeded"))
        }
    })
    .await;

    let asets = assets(&env).await;
    assert_eq!(asets.len(), 10);
    assert!(asets
        .iter()
        .all(|a| a.current_version.as_deref() == Some("2.0.0")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canary_only_issues_first_asset_until_completion() {
    let env = test_env(
        "canary",
        &[
            ("node-01", "edge-linux-x86", "idle", vec!["region=lab"]),
            ("node-02", "edge-linux-x86", "idle", vec!["region=lab"]),
            ("node-03", "edge-linux-x86", "idle", vec!["region=lab"]),
        ],
    )
    .await;

    let (artifact_path, sha256) = make_artifact(&env.root, "artifact.bin", b"release-canary");
    let release = create_release(
        &env,
        "3.0.0",
        "edge-linux-x86",
        Url::from_file_path(artifact_path).unwrap().to_string(),
        Some(sha256),
        "grub-ab",
        vec![sleep_success_check(4)],
    )
    .await;

    let deployment = create_deployment(
        &env,
        &release.id,
        "edge-linux-x86",
        json!({"region": "lab"}),
        vec!["idle"],
        1,
        3,
        1.0,
        true,
    )
    .await;

    wait_until(Duration::from_secs(10), || {
        let env = &env;
        let deployment_id = deployment.id.clone();
        async move {
            let mut ts = targets(env, &deployment_id).await;
            ts.sort_by(|a, b| a.asset_id.cmp(&b.asset_id));
            Some(
                ts.len() == 3
                    && ts[0].state == "issued"
                    && ts[1].state == "pending"
                    && ts[2].state == "pending",
            )
        }
    })
    .await;

    wait_until(Duration::from_secs(30), || {
        let env = &env;
        let deployment_id = deployment.id.clone();
        async move {
            let ts = targets(env, &deployment_id).await;
            Some(ts.iter().all(|t| t.state == "succeeded"))
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn max_parallel_limits_in_flight_work() {
    let env = test_env(
        "max-parallel",
        &[
            ("node-01", "edge-linux-x86", "idle", vec!["region=lab"]),
            ("node-02", "edge-linux-x86", "idle", vec!["region=lab"]),
            ("node-03", "edge-linux-x86", "idle", vec!["region=lab"]),
        ],
    )
    .await;

    let (artifact_path, sha256) = make_artifact(&env.root, "artifact.bin", b"release-max-parallel");
    let release = create_release(
        &env,
        "4.0.0",
        "edge-linux-x86",
        Url::from_file_path(artifact_path).unwrap().to_string(),
        Some(sha256),
        "grub-ab",
        vec![sleep_success_check(4)],
    )
    .await;

    let deployment = create_deployment(
        &env,
        &release.id,
        "edge-linux-x86",
        json!({"region": "lab"}),
        vec!["idle"],
        0,
        1,
        1.0,
        true,
    )
    .await;

    wait_until(Duration::from_secs(10), || {
        let env = &env;
        let deployment_id = deployment.id.clone();
        async move {
            let ts = targets(env, &deployment_id).await;
            let issued = ts.iter().filter(|t| t.state == "issued").count();
            let pending = ts.iter().filter(|t| t.state == "pending").count();
            Some(ts.len() == 3 && issued == 1 && pending == 2)
        }
    })
    .await;

    wait_until(Duration::from_secs(30), || {
        let env = &env;
        let deployment_id = deployment.id.clone();
        async move {
            let ts = targets(env, &deployment_id).await;
            Some(ts.iter().all(|t| t.state == "succeeded"))
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failure_rate_aborts_remaining_rollout() {
    let env = test_env(
        "failure-rate",
        &[
            ("node-01", "edge-linux-x86", "idle", vec!["region=lab"]),
            ("node-02", "edge-linux-x86", "idle", vec!["region=lab"]),
            ("node-03", "edge-linux-x86", "idle", vec!["region=lab"]),
            ("node-04", "edge-linux-x86", "idle", vec!["region=lab"]),
        ],
    )
    .await;

    let (artifact_path, sha256) = make_artifact(&env.root, "artifact.bin", b"release-failure-rate");
    let release = create_release(
        &env,
        "5.0.0",
        "edge-linux-x86",
        Url::from_file_path(artifact_path).unwrap().to_string(),
        Some(sha256),
        "grub-ab",
        vec![fail_check()],
    )
    .await;

    let deployment_record = create_deployment(
        &env,
        &release.id,
        "edge-linux-x86",
        json!({"region": "lab"}),
        vec!["idle"],
        0,
        1,
        0.20,
        true,
    )
    .await;

    wait_until(Duration::from_secs(20), || {
        let env = &env;
        let deployment_id = deployment_record.id.clone();
        async move { Some(deployment(env, &deployment_id).await.status == "aborted") }
    })
    .await;

    let ts = targets(&env, &deployment_record.id).await;
    let failed = ts.iter().filter(|t| t.state == "failed").count();
    let succeeded = ts.iter().filter(|t| t.state == "succeeded").count();
    assert_eq!(
        failed, 1,
        "expected exactly one failed target before abort: {ts:?}"
    );
    assert_eq!(succeeded, 0, "expected no successful targets: {ts:?}");
    assert!(
        ts.iter().filter(|t| t.state == "pending").count() >= 3,
        "expected remaining targets to stay pending after abort: {ts:?}"
    );
    assert!(ts
        .iter()
        .find(|t| t.state == "failed")
        .and_then(|t| t.last_error.clone())
        .is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn require_idle_excludes_busy_assets() {
    let env = test_env(
        "require-idle",
        &[
            ("node-idle", "edge-linux-x86", "idle", vec!["region=lab"]),
            ("node-busy", "edge-linux-x86", "busy", vec!["region=lab"]),
        ],
    )
    .await;

    let (artifact_path, sha256) = make_artifact(&env.root, "artifact.bin", b"release-idle-only");
    let release = create_release(
        &env,
        "6.0.0",
        "edge-linux-x86",
        Url::from_file_path(artifact_path).unwrap().to_string(),
        Some(sha256),
        "grub-ab",
        vec![always_pass_check()],
    )
    .await;

    let deployment = create_deployment(
        &env,
        &release.id,
        "edge-linux-x86",
        json!({"region": "lab"}),
        vec![],
        0,
        2,
        1.0,
        true,
    )
    .await;

    wait_until(Duration::from_secs(15), || {
        let env = &env;
        let deployment_id = deployment.id.clone();
        async move {
            let ts = targets(env, &deployment_id).await;
            Some(ts.len() == 1 && ts[0].asset_id == "node-idle" && ts[0].state == "succeeded")
        }
    })
    .await;

    let asets = assets(&env).await;
    let busy = asets.iter().find(|a| a.asset_id == "node-busy").unwrap();
    assert_eq!(busy.mission_state, "busy");
    assert_eq!(busy.current_version, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scripted_executor_exports_artifact_env_vars() {
    let env = test_env(
        "scripted-env",
        &[("node-01", "edge-linux-x86", "idle", vec!["region=lab"])],
    )
    .await;

    let (artifact_path, sha256) =
        make_artifact(&env.root, "artifact-scripted.bin", b"scripted-env");
    let output_path = env.root.join("scripted-env-output.txt");
    let install_command = format!(
        "printf '%s|%s|%s' \"$ARTIFACT_PATH\" \"$RELEASE_VERSION\" \"$STATE_DIR\" > {}",
        output_path.display()
    );
    let release = create_release_from_manifest(
        &env,
        "7.0.0",
        json!({
            "target_type": "edge-linux-x86",
            "executor": {
                "kind": "scripted",
                "artifact": {
                    "url": Url::from_file_path(&artifact_path).unwrap().to_string(),
                    "sha256": sha256,
                    "headers": {}
                },
                "install_command": install_command,
                "activate_command": Value::Null
            },
            "validation": {
                "health_checks": [always_pass_check()]
            },
            "rollback": {
                "automatic": true,
                "on_boot_failure": true,
                "on_validation_failure": true,
                "candidate_timeout_seconds": 900
            },
            "labels": {"track": "test"}
        }),
    )
    .await;

    let deployment = create_deployment(
        &env,
        &release.id,
        "edge-linux-x86",
        json!({"region": "lab"}),
        vec!["idle"],
        0,
        1,
        1.0,
        true,
    )
    .await;

    wait_until(Duration::from_secs(15), || {
        let env = &env;
        let deployment_id = deployment.id.clone();
        async move {
            let ts = targets(env, &deployment_id).await;
            Some(ts.len() == 1 && ts[0].state == "succeeded")
        }
    })
    .await;

    let written = fs::read_to_string(&output_path).unwrap();
    let parts: Vec<_> = written.split('|').collect();
    assert_eq!(parts.len(), 3);
    assert!(
        parts[0].ends_with("artifact-scripted.bin"),
        "unexpected artifact path: {written}"
    );
    assert_eq!(parts[1], "7.0.0");
    assert!(
        parts[2].contains("state-node-01"),
        "unexpected state dir: {written}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grub_ab_real_mode_writes_inactive_slot_and_reports_success() {
    let root = unique_root("grub-ab-real-success");
    let (server, server_url) = start_server(&root).await;
    let harness = make_grub_ab_harness(&root);
    let client = Client::new();
    let agent = spawn_agent_with_env(
        &server_url,
        &root,
        "node-01",
        "edge-linux-x86",
        "idle",
        &["region=lab"],
        &[
            (
                "NODA_GRUB_AB_ACTIVE_DEVICE_FILE",
                harness.active_device_file.to_str().unwrap(),
            ),
            ("NODA_GRUB_AB_LIVE_ROOT", harness.authority_root.to_str().unwrap()),
            ("NODA_GRUB_EDITENV", harness.grub_editenv_script.to_str().unwrap()),
            ("NODA_MOUNT_COMMAND", harness.mount_script.to_str().unwrap()),
            ("NODA_UMOUNT_COMMAND", harness.umount_script.to_str().unwrap()),
            ("NODA_REBOOT_COMMAND", harness.reboot_script.to_str().unwrap()),
        ],
    );
    let env = TestEnv {
        root: root.clone(),
        server_url,
        _server: server,
        _agents: vec![agent],
        client,
    };

    wait_until(Duration::from_secs(20), || {
        let env = &env;
        async move { Some(assets(env).await.len() == 1) }
    })
    .await;

    let (artifact_path, sha256) =
        make_artifact(&env.root, "grub-ab-image.ext4", b"bootable-rootfs-image-v1");
    let release = create_grub_ab_release(
        &env,
        "8.0.0",
        Url::from_file_path(artifact_path).unwrap().to_string(),
        Some(sha256),
        &harness,
        json!({
            "timeout_seconds": 5,
            "health_checks": [always_pass_check()]
        }),
    )
    .await;

    let deployment = create_deployment(
        &env,
        &release.id,
        "edge-linux-x86",
        json!({"region": "lab"}),
        vec!["idle"],
        0,
        1,
        1.0,
        true,
    )
    .await;

    wait_until(Duration::from_secs(15), || {
        let env = &env;
        let deployment_id = deployment.id.clone();
        async move {
            let ts = targets(env, &deployment_id).await;
            Some(ts.len() == 1 && ts[0].state == "succeeded")
        }
    })
    .await;

    let slot_b = fs::read(&harness.slot_b).unwrap();
    assert_eq!(slot_b, b"bootable-rootfs-image-v1");

    let asets = assets(&env).await;
    assert_eq!(asets[0].current_version.as_deref(), Some("8.0.0"));
    assert_eq!(asets[0].active_slot.as_deref(), Some("B"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grub_ab_real_mode_rolls_back_after_validation_failure() {
    let root = unique_root("grub-ab-real-rollback");
    let (server, server_url) = start_server(&root).await;
    let harness = make_grub_ab_harness(&root);
    let client = Client::new();
    let agent = spawn_agent_with_env(
        &server_url,
        &root,
        "node-01",
        "edge-linux-x86",
        "idle",
        &["region=lab"],
        &[
            (
                "NODA_GRUB_AB_ACTIVE_DEVICE_FILE",
                harness.active_device_file.to_str().unwrap(),
            ),
            ("NODA_GRUB_AB_LIVE_ROOT", harness.authority_root.to_str().unwrap()),
            ("NODA_GRUB_EDITENV", harness.grub_editenv_script.to_str().unwrap()),
            ("NODA_MOUNT_COMMAND", harness.mount_script.to_str().unwrap()),
            ("NODA_UMOUNT_COMMAND", harness.umount_script.to_str().unwrap()),
            ("NODA_REBOOT_COMMAND", harness.reboot_script.to_str().unwrap()),
        ],
    );
    let env = TestEnv {
        root: root.clone(),
        server_url,
        _server: server,
        _agents: vec![agent],
        client,
    };

    wait_until(Duration::from_secs(20), || {
        let env = &env;
        async move { Some(assets(env).await.len() == 1) }
    })
    .await;

    let (artifact_path, sha256) =
        make_artifact(&env.root, "grub-ab-image.ext4", b"bootable-rootfs-image-v2");
    let release = create_grub_ab_release(
        &env,
        "9.0.0",
        Url::from_file_path(artifact_path).unwrap().to_string(),
        Some(sha256),
        &harness,
        json!({
            "expected_hostname": "definitely-not-this-host",
            "timeout_seconds": 1,
            "health_checks": [always_pass_check()]
        }),
    )
    .await;

    let deployment = create_deployment(
        &env,
        &release.id,
        "edge-linux-x86",
        json!({"region": "lab"}),
        vec!["idle"],
        0,
        1,
        1.0,
        true,
    )
    .await;

    wait_until(Duration::from_secs(20), || {
        let env = &env;
        let deployment_id = deployment.id.clone();
        async move {
            let ts = targets(env, &deployment_id).await;
            Some(ts.len() == 1 && ts[0].state == "failed")
        }
    })
    .await;

    let active_device = fs::read_to_string(&harness.active_device_file).unwrap();
    assert_eq!(active_device.trim(), harness.slot_a.display().to_string());

    let asets = assets(&env).await;
    assert_eq!(asets[0].current_version, None);
    assert_eq!(asets[0].active_slot.as_deref(), Some("A"));
}
