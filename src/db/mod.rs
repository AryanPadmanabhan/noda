use crate::types::*;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::{collections::BTreeMap, path::Path};
use uuid::Uuid;

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("opening db at {}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS releases (
            id TEXT PRIMARY KEY,
            version TEXT NOT NULL,
            target_type TEXT NOT NULL,
            manifest_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS assets (
            asset_id TEXT PRIMARY KEY,
            asset_type TEXT NOT NULL,
            mission_state TEXT NOT NULL,
            current_version TEXT,
            desired_version TEXT,
            active_slot TEXT,
            status TEXT NOT NULL,
            last_seen TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS asset_labels (
            asset_id TEXT NOT NULL,
            label TEXT NOT NULL,
            PRIMARY KEY(asset_id, label),
            FOREIGN KEY(asset_id) REFERENCES assets(asset_id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS deployments (
            id TEXT PRIMARY KEY,
            release_id TEXT NOT NULL,
            rollout_name TEXT NOT NULL,
            status TEXT NOT NULL,
            selector_json TEXT NOT NULL,
            strategy_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(release_id) REFERENCES releases(id)
        );
        CREATE TABLE IF NOT EXISTS deployment_targets (
            deployment_id TEXT NOT NULL,
            asset_id TEXT NOT NULL,
            state TEXT NOT NULL,
            last_error TEXT,
            current_command_id TEXT,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(deployment_id, asset_id),
            FOREIGN KEY(deployment_id) REFERENCES deployments(id) ON DELETE CASCADE,
            FOREIGN KEY(asset_id) REFERENCES assets(asset_id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS commands (
            id TEXT PRIMARY KEY,
            deployment_id TEXT NOT NULL,
            release_id TEXT NOT NULL,
            asset_id TEXT NOT NULL,
            command_type TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(deployment_id) REFERENCES deployments(id) ON DELETE CASCADE,
            FOREIGN KEY(release_id) REFERENCES releases(id),
            FOREIGN KEY(asset_id) REFERENCES assets(asset_id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS command_results (
            command_id TEXT PRIMARY KEY,
            success INTEGER NOT NULL,
            message TEXT NOT NULL,
            active_slot TEXT,
            booted_version TEXT,
            created_at TEXT NOT NULL,
            FOREIGN KEY(command_id) REFERENCES commands(id) ON DELETE CASCADE
        );
        "#,
    )?;
    Ok(())
}

pub fn insert_release(conn: &Connection, req: CreateReleaseRequest) -> Result<ReleaseRecord> {
    let id = Uuid::new_v4().to_string();
    let created_at = Utc::now();
    let manifest_json = serde_json::to_string(&req.manifest)?;
    conn.execute(
        "INSERT INTO releases (id, version, target_type, manifest_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, req.version, req.manifest.target_type, manifest_json, created_at.to_rfc3339()],
    )?;
    Ok(ReleaseRecord {
        id,
        version: req.version,
        target_type: req.manifest.target_type.clone(),
        manifest: req.manifest,
        created_at,
    })
}

pub fn list_releases(conn: &Connection) -> Result<Vec<ReleaseRecord>> {
    let mut stmt = conn.prepare("SELECT id, version, target_type, manifest_json, created_at FROM releases ORDER BY created_at DESC")?;
    let rows = stmt.query_map([], map_release)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

pub fn get_release(conn: &Connection, id: &str) -> Result<ReleaseRecord> {
    conn.query_row(
        "SELECT id, version, target_type, manifest_json, created_at FROM releases WHERE id = ?1",
        params![id],
        map_release,
    )
    .with_context(|| format!("release not found: {id}"))
}

fn map_release(row: &Row<'_>) -> rusqlite::Result<ReleaseRecord> {
    let manifest_json: String = row.get(3)?;
    let created_at: String = row.get(4)?;
    Ok(ReleaseRecord {
        id: row.get(0)?,
        version: row.get(1)?,
        target_type: row.get(2)?,
        manifest: serde_json::from_str(&manifest_json).map_err(to_sql_err)?,
        created_at: parse_ts(&created_at).map_err(to_sql_err)?,
    })
}

pub fn upsert_asset(conn: &Connection, req: AgentCheckinRequest) -> Result<AssetRecord> {
    let now = Utc::now();
    let status = req.status.clone().unwrap_or_else(|| "online".into());
    conn.execute(
        r#"
        INSERT INTO assets (asset_id, asset_type, mission_state, current_version, desired_version, active_slot, status, last_seen)
        VALUES (?1, ?2, ?3, ?4, COALESCE((SELECT desired_version FROM assets WHERE asset_id = ?1), NULL), ?5, ?6, ?7)
        ON CONFLICT(asset_id) DO UPDATE SET
            asset_type = excluded.asset_type,
            mission_state = excluded.mission_state,
            current_version = COALESCE(excluded.current_version, assets.current_version),
            active_slot = COALESCE(excluded.active_slot, assets.active_slot),
            status = excluded.status,
            last_seen = excluded.last_seen
        "#,
        params![
            req.asset_id,
            req.asset_type,
            req.mission_state,
            req.current_version,
            req.active_slot,
            status,
            now.to_rfc3339(),
        ],
    )?;
    conn.execute("DELETE FROM asset_labels WHERE asset_id = ?1", params![req.asset_id])?;
    for label in &req.labels {
        conn.execute(
            "INSERT INTO asset_labels (asset_id, label) VALUES (?1, ?2)",
            params![req.asset_id, label],
        )?;
    }
    get_asset(conn, &req.asset_id)
}

pub fn list_assets(conn: &Connection) -> Result<Vec<AssetRecord>> {
    let mut stmt = conn.prepare(
        "SELECT asset_id, asset_type, mission_state, current_version, desired_version, active_slot, status, last_seen FROM assets ORDER BY last_seen DESC",
    )?;
    let rows = stmt.query_map([], |row| map_asset(conn, row))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

pub fn get_asset(conn: &Connection, id: &str) -> Result<AssetRecord> {
    conn.query_row(
        "SELECT asset_id, asset_type, mission_state, current_version, desired_version, active_slot, status, last_seen FROM assets WHERE asset_id = ?1",
        params![id],
        |row| map_asset(conn, row),
    )
    .with_context(|| format!("asset not found: {id}"))
}

fn map_asset(conn: &Connection, row: &Row<'_>) -> rusqlite::Result<AssetRecord> {
    let asset_id: String = row.get(0)?;
    let labels = get_asset_labels(conn, &asset_id).map_err(to_sql_err)?;
    let ts: String = row.get(7)?;
    Ok(AssetRecord {
        asset_id,
        asset_type: row.get(1)?,
        mission_state: row.get(2)?,
        current_version: row.get(3)?,
        desired_version: row.get(4)?,
        active_slot: row.get(5)?,
        status: row.get(6)?,
        last_seen: parse_ts(&ts).map_err(to_sql_err)?,
        labels,
    })
}

fn get_asset_labels(conn: &Connection, asset_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT label FROM asset_labels WHERE asset_id = ?1 ORDER BY label ASC")?;
    let rows = stmt.query_map(params![asset_id], |row| row.get(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

pub fn create_deployment(conn: &Connection, req: CreateDeploymentRequest) -> Result<DeploymentRecord> {
    let release = get_release(conn, &req.release_id)?;
    if release.target_type != req.selector.target_type {
        return Err(anyhow!("release target_type and selector target_type mismatch"));
    }
    let id = Uuid::new_v4().to_string();
    let created_at = Utc::now();
    let selector_json = serde_json::to_string(&req.selector)?;
    let strategy_json = serde_json::to_string(&req.strategy)?;
    conn.execute(
        "INSERT INTO deployments (id, release_id, rollout_name, status, selector_json, strategy_json, created_at) VALUES (?1, ?2, ?3, 'active', ?4, ?5, ?6)",
        params![id, req.release_id, req.rollout_name, selector_json, strategy_json, created_at.to_rfc3339()],
    )?;

    let assets = select_assets_for_deployment(conn, &req.selector, req.strategy.require_idle)?;
    for asset in assets {
        conn.execute(
            "UPDATE assets SET desired_version = ?2 WHERE asset_id = ?1",
            params![asset.asset_id, release.version],
        )?;
        conn.execute(
            "INSERT INTO deployment_targets (deployment_id, asset_id, state, updated_at) VALUES (?1, ?2, 'pending', ?3)",
            params![id, asset.asset_id, created_at.to_rfc3339()],
        )?;
    }

    get_deployment(conn, &id)
}

pub fn list_deployments(conn: &Connection) -> Result<Vec<DeploymentRecord>> {
    let mut stmt = conn.prepare("SELECT id, release_id, rollout_name, status, selector_json, strategy_json, created_at FROM deployments ORDER BY created_at DESC")?;
    let rows = stmt.query_map([], map_deployment)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

pub fn get_deployment(conn: &Connection, id: &str) -> Result<DeploymentRecord> {
    conn.query_row(
        "SELECT id, release_id, rollout_name, status, selector_json, strategy_json, created_at FROM deployments WHERE id = ?1",
        params![id],
        map_deployment,
    )
    .with_context(|| format!("deployment not found: {id}"))
}

fn map_deployment(row: &Row<'_>) -> rusqlite::Result<DeploymentRecord> {
    let selector_json: String = row.get(4)?;
    let strategy_json: String = row.get(5)?;
    let created_at: String = row.get(6)?;
    Ok(DeploymentRecord {
        id: row.get(0)?,
        release_id: row.get(1)?,
        rollout_name: row.get(2)?,
        status: row.get(3)?,
        selector: serde_json::from_str(&selector_json).map_err(to_sql_err)?,
        strategy: serde_json::from_str(&strategy_json).map_err(to_sql_err)?,
        created_at: parse_ts(&created_at).map_err(to_sql_err)?,
    })
}

pub fn list_deployment_targets(conn: &Connection, deployment_id: &str) -> Result<Vec<DeploymentTargetRecord>> {
    let mut stmt = conn.prepare(
        "SELECT deployment_id, asset_id, state, last_error, current_command_id, updated_at FROM deployment_targets WHERE deployment_id = ?1 ORDER BY updated_at DESC"
    )?;
    let rows = stmt.query_map(params![deployment_id], |row| {
        let ts: String = row.get(5)?;
        Ok(DeploymentTargetRecord {
            deployment_id: row.get(0)?,
            asset_id: row.get(1)?,
            state: row.get(2)?,
            last_error: row.get(3)?,
            current_command_id: row.get(4)?,
            updated_at: parse_ts(&ts).map_err(to_sql_err)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
}

pub fn set_deployment_paused(conn: &Connection, deployment_id: &str, paused: bool) -> Result<()> {
    let status = if paused { "paused" } else { "active" };
    conn.execute(
        "UPDATE deployments SET status = ?2 WHERE id = ?1",
        params![deployment_id, status],
    )?;
    Ok(())
}

pub fn abort_deployment(conn: &Connection, deployment_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE deployments SET status = 'aborted' WHERE id = ?1",
        params![deployment_id],
    )?;
    Ok(())
}

fn select_assets_for_deployment(conn: &Connection, selector: &Selector, require_idle: bool) -> Result<Vec<AssetRecord>> {
    let assets = list_assets(conn)?;
    let filtered = assets
        .into_iter()
        .filter(|a| a.asset_type == selector.target_type)
        .filter(|a| selector.labels.iter().all(|(k, v)| a.labels.iter().any(|label| label == &format!("{k}={v}"))))
        .filter(|a| selector.mission_states.is_empty() || selector.mission_states.iter().any(|s| s == &a.mission_state))
        .filter(|a| !require_idle || a.mission_state == "idle")
        .collect();
    Ok(filtered)
}

pub fn poll_commands(conn: &Connection, asset_id: &str) -> Result<Vec<CommandRecord>> {
    let asset = get_asset(conn, asset_id)?;
    let active_deployments = list_deployments(conn)?
        .into_iter()
        .filter(|d| d.status == "active")
        .collect::<Vec<_>>();

    let mut out = Vec::new();
    for dep in active_deployments {
        let targets = list_deployment_targets(conn, &dep.id)?;
        let Some(target) = targets.into_iter().find(|t| t.asset_id == asset_id) else { continue };

        if target.state != "pending" && target.state != "retry" {
            continue;
        }

        let stats = deployment_stats(conn, &dep.id)?;
        if stats.failure_rate > dep.strategy.max_failure_rate {
            conn.execute("UPDATE deployments SET status = 'aborted' WHERE id = ?1", params![dep.id.clone()])?;
            continue;
        }

        let active_commands = count_active_commands(conn, &dep.id)?;
        if active_commands >= dep.strategy.max_parallel {
            continue;
        }

        // canary gate: if canary configured and no completions yet, issue only first N assets alphabetically.
        let mut all_targets = list_deployment_targets(conn, &dep.id)?;
        all_targets.sort_by(|a, b| a.asset_id.cmp(&b.asset_id));
        if dep.strategy.canary > 0 {
            let completed = all_targets
                .iter()
                .filter(|t| matches!(t.state.as_str(), "succeeded" | "failed" | "rolled_back"))
                .count();
            let canary_assets: Vec<String> = all_targets.iter().take(dep.strategy.canary).map(|t| t.asset_id.clone()).collect();
            if completed < dep.strategy.canary && !canary_assets.contains(&asset.asset_id) {
                continue;
            }
        }

        let release = get_release(conn, &dep.release_id)?;
        let cmd_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO commands (id, deployment_id, release_id, asset_id, command_type, status, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'install_release', 'queued', ?5, ?5)",
            params![cmd_id, dep.id, dep.release_id, asset_id, now],
        )?;
        conn.execute(
            "UPDATE deployment_targets SET state = 'issued', current_command_id = ?3, updated_at = ?4 WHERE deployment_id = ?1 AND asset_id = ?2",
            params![dep.id, asset_id, cmd_id, now],
        )?;
        out.push(CommandRecord {
            id: cmd_id,
            deployment_id: dep.id,
            release_id: release.id.clone(),
            asset_id: asset_id.to_string(),
            command_type: "install_release".into(),
            status: "queued".into(),
            manifest: release.manifest,
            release_version: release.version,
        });
    }

    Ok(out)
}

fn count_active_commands(conn: &Connection, deployment_id: &str) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM commands WHERE deployment_id = ?1 AND status IN ('queued', 'running')",
        params![deployment_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

struct DeploymentStats {
    failure_rate: f64,
}

fn deployment_stats(conn: &Connection, deployment_id: &str) -> Result<DeploymentStats> {
    let mut stmt = conn.prepare("SELECT state FROM deployment_targets WHERE deployment_id = ?1")?;
    let states = stmt.query_map(params![deployment_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let total = states.len();
    if total == 0 {
        return Ok(DeploymentStats { failure_rate: 0.0 });
    }
    let failed = states.iter().filter(|s| matches!(s.as_str(), "failed" | "rolled_back")).count();
    Ok(DeploymentStats { failure_rate: failed as f64 / total as f64 })
}

pub fn mark_command_running(conn: &Connection, command_id: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE commands SET status = 'running', updated_at = ?2 WHERE id = ?1",
        params![command_id, now],
    )?;
    Ok(())
}

pub fn submit_command_result(conn: &Connection, req: AgentResultRequest) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO command_results (command_id, success, message, active_slot, booted_version, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![req.command_id, req.success as i64, req.message, req.active_slot, req.booted_version, now],
    )?;

    let command = conn.query_row(
        "SELECT deployment_id, asset_id FROM commands WHERE id = ?1",
        params![req.command_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    )?;

    conn.execute(
        "UPDATE commands SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![req.command_id, if req.success { "succeeded" } else { "failed" }, now],
    )?;
    conn.execute(
        "UPDATE deployment_targets SET state = ?3, last_error = ?4, updated_at = ?5 WHERE deployment_id = ?1 AND asset_id = ?2",
        params![command.0, command.1, if req.success { "succeeded" } else { "failed" }, if req.success { None::<String> } else { Some(req.message.clone()) }, now],
    )?;
    conn.execute(
        "UPDATE assets SET current_version = COALESCE(?2, current_version), active_slot = COALESCE(?3, active_slot), desired_version = NULL WHERE asset_id = ?1",
        params![command.1, req.booted_version, req.active_slot],
    )?;
    Ok(())
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(raw)?.with_timezone(&Utc))
}

fn to_sql_err<E: std::fmt::Display>(err: E) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string())),
    )
}
