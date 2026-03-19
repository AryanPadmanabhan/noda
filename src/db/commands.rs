use super::{
    assets::get_asset,
    deployments::{deployment_stats, list_deployment_targets},
    releases::get_release,
};
use crate::types::{
    AgentResultRequest, CommandRecord, CommandStatus, DeploymentStatus, DeploymentTargetState,
};
use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection};
use uuid::Uuid;

pub fn poll_commands(conn: &Connection, asset_id: &str) -> Result<Vec<CommandRecord>> {
    let asset = get_asset(conn, asset_id)?;
    let active_deployments = super::deployments::list_deployments(conn)?
        .into_iter()
        .filter(|deployment| deployment.status == DeploymentStatus::Active)
        .collect::<Vec<_>>();

    let mut commands = Vec::new();
    for deployment in active_deployments {
        let targets = list_deployment_targets(conn, &deployment.id)?;
        let Some(target) = targets
            .into_iter()
            .find(|target| target.asset_id == asset_id)
        else {
            continue;
        };

        if target.state != DeploymentTargetState::Pending
            && target.state != DeploymentTargetState::Retry
        {
            continue;
        }

        let stats = deployment_stats(conn, &deployment.id)?;
        if stats.failure_rate > deployment.strategy.max_failure_rate {
            conn.execute(
                "UPDATE deployments SET status = ?2 WHERE id = ?1",
                params![deployment.id.clone(), DeploymentStatus::Aborted.as_str()],
            )?;
            continue;
        }

        if count_active_commands(conn, &deployment.id)? >= deployment.strategy.max_parallel {
            continue;
        }

        let mut all_targets = list_deployment_targets(conn, &deployment.id)?;
        all_targets.sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
        if deployment.strategy.canary > 0 {
            let completed = all_targets
                .iter()
                .filter(|target| {
                    matches!(
                        target.state,
                        DeploymentTargetState::Succeeded
                            | DeploymentTargetState::Failed
                            | DeploymentTargetState::RolledBack
                    )
                })
                .count();
            let canary_assets = all_targets
                .iter()
                .take(deployment.strategy.canary)
                .map(|target| target.asset_id.clone())
                .collect::<Vec<_>>();
            if completed < deployment.strategy.canary && !canary_assets.contains(&asset.asset_id) {
                continue;
            }
        }

        let release = get_release(conn, &deployment.release_id)?;
        let command_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO commands (id, deployment_id, release_id, asset_id, command_type, status, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                command_id,
                deployment.id,
                deployment.release_id,
                asset_id,
                "install_release",
                CommandStatus::Queued.as_str(),
                now
            ],
        )?;
        conn.execute(
            "UPDATE deployment_targets SET state = ?3, current_command_id = ?4, updated_at = ?5 WHERE deployment_id = ?1 AND asset_id = ?2",
            params![
                deployment.id,
                asset_id,
                DeploymentTargetState::Issued.as_str(),
                command_id,
                now
            ],
        )?;
        commands.push(CommandRecord {
            id: command_id,
            deployment_id: deployment.id,
            release_id: release.id.clone(),
            asset_id: asset_id.to_string(),
            command_type: "install_release".into(),
            status: CommandStatus::Queued,
            manifest: release.manifest,
            release_version: release.version,
        });
    }

    Ok(commands)
}

pub fn mark_command_running(conn: &Connection, command_id: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE commands SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![command_id, CommandStatus::Running.as_str(), now],
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
        params![
            req.command_id,
            if req.success {
                CommandStatus::Succeeded.as_str()
            } else {
                CommandStatus::Failed.as_str()
            },
            now
        ],
    )?;
    conn.execute(
        "UPDATE deployment_targets SET state = ?3, last_error = ?4, updated_at = ?5 WHERE deployment_id = ?1 AND asset_id = ?2",
        params![
            command.0,
            command.1,
            if req.success {
                DeploymentTargetState::Succeeded.as_str()
            } else {
                DeploymentTargetState::Failed.as_str()
            },
            if req.success { None::<String> } else { Some(req.message.clone()) },
            now
        ],
    )?;
    conn.execute(
        "UPDATE assets SET current_version = COALESCE(?2, current_version), active_slot = COALESCE(?3, active_slot), desired_version = NULL WHERE asset_id = ?1",
        params![command.1, req.booted_version, req.active_slot],
    )?;
    Ok(())
}

fn count_active_commands(conn: &Connection, deployment_id: &str) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM commands WHERE deployment_id = ?1 AND status IN ('queued', 'running')",
        params![deployment_id],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}
