use crate::{db, server::AppState, types::*};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/releases", post(create_release).get(list_releases))
        .route("/v1/releases/:id", get(get_release))
        .route("/v1/assets", get(list_assets))
        .route("/v1/assets/:id", get(get_asset))
        .route("/v1/deployments", post(create_deployment).get(list_deployments))
        .route("/v1/deployments/:id", get(get_deployment))
        .route("/v1/deployments/:id/targets", get(list_deployment_targets))
        .route("/v1/deployments/:id/pause", post(pause_deployment))
        .route("/v1/deployments/:id/abort", post(abort_deployment))
        .route("/v1/agent/checkin", post(agent_checkin))
        .route("/v1/agent/poll", post(agent_poll))
        .route("/v1/agent/result", post(agent_result))
}

async fn healthz() -> &'static str {
    "ok"
}

async fn create_release(
    State(state): State<AppState>,
    Json(req): Json<CreateReleaseRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    let record = db::insert_release(&conn, req).map_err(internal)?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn list_releases(State(state): State<AppState>) -> Result<Json<Vec<ReleaseRecord>>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    let records = db::list_releases(&conn).map_err(internal)?;
    Ok(Json(records))
}

async fn get_release(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ReleaseRecord>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    let record = db::get_release(&conn, &id).map_err(not_found_or_internal)?;
    Ok(Json(record))
}

async fn list_assets(State(state): State<AppState>) -> Result<Json<Vec<AssetRecord>>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    Ok(Json(db::list_assets(&conn).map_err(internal)?))
}

async fn get_asset(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<AssetRecord>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    Ok(Json(db::get_asset(&conn, &id).map_err(not_found_or_internal)?))
}

async fn create_deployment(
    State(state): State<AppState>,
    Json(req): Json<CreateDeploymentRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    let record = db::create_deployment(&conn, req).map_err(internal)?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn list_deployments(State(state): State<AppState>) -> Result<Json<Vec<DeploymentRecord>>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    Ok(Json(db::list_deployments(&conn).map_err(internal)?))
}

async fn get_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DeploymentRecord>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    Ok(Json(db::get_deployment(&conn, &id).map_err(not_found_or_internal)?))
}

async fn list_deployment_targets(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<DeploymentTargetRecord>>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    Ok(Json(db::list_deployment_targets(&conn, &id).map_err(internal)?))
}

async fn pause_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PauseDeploymentRequest>,
) -> Result<Json<ApiMessage>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    db::set_deployment_paused(&conn, &id, req.paused).map_err(internal)?;
    Ok(Json(ApiMessage { message: if req.paused { "paused".into() } else { "resumed".into() } }))
}

async fn abort_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ApiMessage>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    db::abort_deployment(&conn, &id).map_err(internal)?;
    Ok(Json(ApiMessage { message: "aborted".into() }))
}

async fn agent_checkin(
    State(state): State<AppState>,
    Json(req): Json<AgentCheckinRequest>,
) -> Result<Json<AssetRecord>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    Ok(Json(db::upsert_asset(&conn, req).map_err(internal)?))
}

async fn agent_poll(
    State(state): State<AppState>,
    Json(req): Json<AgentPollRequest>,
) -> Result<Json<AgentPollResponse>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    let commands = db::poll_commands(&conn, &req.asset_id).map_err(internal)?;
    for cmd in &commands {
        db::mark_command_running(&conn, &cmd.id).map_err(internal)?;
    }
    Ok(Json(AgentPollResponse { commands }))
}

async fn agent_result(
    State(state): State<AppState>,
    Json(req): Json<AgentResultRequest>,
) -> Result<Json<ApiMessage>, (StatusCode, String)> {
    let conn = state.db.lock().unwrap();
    db::submit_command_result(&conn, req).map_err(internal)?;
    Ok(Json(ApiMessage { message: "recorded".into() }))
}

fn internal<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn not_found_or_internal<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    let msg = err.to_string();
    if msg.contains("not found") {
        (StatusCode::NOT_FOUND, msg)
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, msg)
    }
}
