use super::{
    assets::list_assets,
    shared::{parse_ts, to_sql_err},
};
use crate::types::{
    CreateDeploymentRequest, DeploymentRecord, DeploymentStatus, DeploymentTargetRecord,
    DeploymentTargetState, Selector,
};
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, Row};
use uuid::Uuid;

pub fn create_deployment(
    conn: &Connection,
    req: CreateDeploymentRequest,
) -> Result<DeploymentRecord> {
    let release = super::releases::get_release(conn, &req.release_id)?;
    if release.target_type != req.selector.target_type {
        return Err(anyhow!(
            "release target_type and selector target_type mismatch"
        ));
    }

    let deployment_id = Uuid::new_v4().to_string();
    let created_at = Utc::now();
    let selector_json = serde_json::to_string(&req.selector)?;
    let strategy_json = serde_json::to_string(&req.strategy)?;
    conn.execute(
        "INSERT INTO deployments (id, release_id, rollout_name, status, selector_json, strategy_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            deployment_id,
            req.release_id,
            req.rollout_name,
            DeploymentStatus::Active.as_str(),
            selector_json,
            strategy_json,
            created_at.to_rfc3339()
        ],
    )?;

    let selected_assets =
        select_assets_for_deployment(conn, &req.selector, req.strategy.require_idle)?;
    for asset in selected_assets {
        conn.execute(
            "UPDATE assets SET desired_version = ?2 WHERE asset_id = ?1",
            params![asset.asset_id, release.version],
        )?;
        conn.execute(
            "INSERT INTO deployment_targets (deployment_id, asset_id, state, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                deployment_id,
                asset.asset_id,
                DeploymentTargetState::Pending.as_str(),
                created_at.to_rfc3339()
            ],
        )?;
    }

    get_deployment(conn, &deployment_id)
}

pub fn list_deployments(conn: &Connection) -> Result<Vec<DeploymentRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, release_id, rollout_name, status, selector_json, strategy_json, created_at FROM deployments ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], map_deployment)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn get_deployment(conn: &Connection, id: &str) -> Result<DeploymentRecord> {
    conn.query_row(
        "SELECT id, release_id, rollout_name, status, selector_json, strategy_json, created_at FROM deployments WHERE id = ?1",
        params![id],
        map_deployment,
    )
    .with_context(|| format!("deployment not found: {id}"))
}

pub fn list_deployment_targets(
    conn: &Connection,
    deployment_id: &str,
) -> Result<Vec<DeploymentTargetRecord>> {
    let mut stmt = conn.prepare(
        "SELECT deployment_id, asset_id, state, last_error, current_command_id, updated_at FROM deployment_targets WHERE deployment_id = ?1 ORDER BY updated_at DESC",
    )?;
    let rows = stmt.query_map(params![deployment_id], |row| {
        let updated_at: String = row.get(5)?;
        Ok(DeploymentTargetRecord {
            deployment_id: row.get(0)?,
            asset_id: row.get(1)?,
            state: DeploymentTargetState::parse(&row.get::<_, String>(2)?).map_err(to_sql_err)?,
            last_error: row.get(3)?,
            current_command_id: row.get(4)?,
            updated_at: parse_ts(&updated_at).map_err(to_sql_err)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn set_deployment_paused(conn: &Connection, deployment_id: &str, paused: bool) -> Result<()> {
    let status = if paused {
        DeploymentStatus::Paused
    } else {
        DeploymentStatus::Active
    };
    conn.execute(
        "UPDATE deployments SET status = ?2 WHERE id = ?1",
        params![deployment_id, status.as_str()],
    )?;
    Ok(())
}

pub fn abort_deployment(conn: &Connection, deployment_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE deployments SET status = ?2 WHERE id = ?1",
        params![deployment_id, DeploymentStatus::Aborted.as_str()],
    )?;
    Ok(())
}

pub(crate) struct DeploymentStats {
    pub failure_rate: f64,
}

pub(crate) fn deployment_stats(conn: &Connection, deployment_id: &str) -> Result<DeploymentStats> {
    let mut stmt = conn.prepare("SELECT state FROM deployment_targets WHERE deployment_id = ?1")?;
    let states = stmt
        .query_map(params![deployment_id], |row| {
            DeploymentTargetState::parse(&row.get::<_, String>(0)?).map_err(to_sql_err)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let total = states.len();
    if total == 0 {
        return Ok(DeploymentStats { failure_rate: 0.0 });
    }
    let failed = states
        .iter()
        .filter(|state| {
            matches!(
                state,
                DeploymentTargetState::Failed | DeploymentTargetState::RolledBack
            )
        })
        .count();
    Ok(DeploymentStats {
        failure_rate: failed as f64 / total as f64,
    })
}

fn map_deployment(row: &Row<'_>) -> rusqlite::Result<DeploymentRecord> {
    let selector_json: String = row.get(4)?;
    let strategy_json: String = row.get(5)?;
    let created_at: String = row.get(6)?;
    Ok(DeploymentRecord {
        id: row.get(0)?,
        release_id: row.get(1)?,
        rollout_name: row.get(2)?,
        status: DeploymentStatus::parse(&row.get::<_, String>(3)?).map_err(to_sql_err)?,
        selector: serde_json::from_str(&selector_json).map_err(to_sql_err)?,
        strategy: serde_json::from_str(&strategy_json).map_err(to_sql_err)?,
        created_at: parse_ts(&created_at).map_err(to_sql_err)?,
    })
}

fn select_assets_for_deployment(
    conn: &Connection,
    selector: &Selector,
    require_idle: bool,
) -> Result<Vec<crate::types::AssetRecord>> {
    let filtered = list_assets(conn)?
        .into_iter()
        .filter(|asset| asset.asset_type == selector.target_type)
        .filter(|asset| {
            selector.labels.iter().all(|(key, value)| {
                asset
                    .labels
                    .iter()
                    .any(|label| label == &format!("{key}={value}"))
            })
        })
        .filter(|asset| {
            selector.mission_states.is_empty()
                || selector
                    .mission_states
                    .iter()
                    .any(|mission_state| mission_state == &asset.mission_state)
        })
        .filter(|asset| !require_idle || asset.mission_state == "idle")
        .collect();
    Ok(filtered)
}
