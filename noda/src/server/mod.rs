use crate::{api, db};
use anyhow::Result;
use axum::Router;
use rusqlite::Connection;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tracing::info;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
}

pub async fn run(bind: String, db_path: PathBuf) -> Result<()> {
    let conn = db::open(&db_path)?;
    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
    };
    let app = Router::new().merge(api::router()).with_state(state);

    let addr: SocketAddr = bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, db = %db_path.display(), "server listening");
    axum::serve(listener, app).await?;
    Ok(())
}
