use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

pub fn get_asset_labels(conn: &Connection, asset_id: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT label FROM asset_labels WHERE asset_id = ?1 ORDER BY label ASC")?;
    let rows = stmt.query_map(params![asset_id], |row| row.get(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(raw)?.with_timezone(&Utc))
}

pub fn to_sql_err<E: std::fmt::Display>(err: E) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            err.to_string(),
        )),
    )
}
