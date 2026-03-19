use super::shared::{parse_ts, to_sql_err};
use crate::types::{CreateReleaseRequest, ReleaseRecord};
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, Row};
use uuid::Uuid;

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
    let mut stmt = conn.prepare(
        "SELECT id, version, target_type, manifest_json, created_at FROM releases ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], map_release)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
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
