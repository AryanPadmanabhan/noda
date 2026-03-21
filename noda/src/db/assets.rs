use super::shared::{get_asset_labels, parse_ts, to_sql_err};
use crate::types::{AgentCheckinRequest, AssetRecord, AssetStatus};
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, Row};

pub fn upsert_asset(conn: &Connection, req: AgentCheckinRequest) -> Result<AssetRecord> {
    let now = Utc::now();
    let status = req.status.unwrap_or(AssetStatus::Online);
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
            status.as_str(),
            now.to_rfc3339(),
        ],
    )?;
    conn.execute(
        "DELETE FROM asset_labels WHERE asset_id = ?1",
        params![req.asset_id],
    )?;
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
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
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
        status: AssetStatus::parse(&row.get::<_, String>(6)?).map_err(to_sql_err)?,
        last_seen: parse_ts(&ts).map_err(to_sql_err)?,
        labels,
    })
}
