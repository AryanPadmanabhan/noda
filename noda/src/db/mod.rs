mod assets;
mod commands;
mod deployments;
mod releases;
mod shared;

pub use assets::{get_asset, list_assets, upsert_asset};
pub use commands::{mark_command_running, poll_commands, submit_command_result};
pub use deployments::{
    abort_deployment, create_deployment, get_deployment, list_deployment_targets, list_deployments,
    set_deployment_paused,
};
pub use releases::{get_release, insert_release, list_releases};

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

pub fn open(path: &Path) -> Result<Connection> {
    let conn =
        Connection::open(path).with_context(|| format!("opening db at {}", path.display()))?;
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
