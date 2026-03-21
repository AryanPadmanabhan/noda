mod error;

use crate::{db, server::AppState, types::*};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use error::{ApiError, ApiResult};
use rusqlite::Connection;
use std::sync::{MutexGuard, PoisonError};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/releases", post(create_release).get(list_releases))
        .route("/v1/releases/:id", get(get_release))
        .route("/v1/assets", get(list_assets))
        .route("/v1/assets/:id", get(get_asset))
        .route(
            "/v1/deployments",
            post(create_deployment).get(list_deployments),
        )
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
) -> ApiResult<(StatusCode, Json<ReleaseRecord>)> {
    req.validate()
        .map_err(|err| ApiError::invalid(err.to_string()))?;
    let conn = db_conn(&state)?;
    let record = db::insert_release(&conn, req).map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn list_releases(State(state): State<AppState>) -> ApiResult<Json<Vec<ReleaseRecord>>> {
    let conn = db_conn(&state)?;
    let records = db::list_releases(&conn).map_err(map_db_error)?;
    Ok(Json(records))
}

async fn get_release(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ReleaseRecord>> {
    let conn = db_conn(&state)?;
    let record = db::get_release(&conn, &id).map_err(map_db_error)?;
    Ok(Json(record))
}

async fn list_assets(State(state): State<AppState>) -> ApiResult<Json<Vec<AssetRecord>>> {
    let conn = db_conn(&state)?;
    Ok(Json(db::list_assets(&conn).map_err(map_db_error)?))
}

async fn get_asset(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<AssetRecord>> {
    let conn = db_conn(&state)?;
    Ok(Json(db::get_asset(&conn, &id).map_err(map_db_error)?))
}

async fn create_deployment(
    State(state): State<AppState>,
    Json(req): Json<CreateDeploymentRequest>,
) -> ApiResult<(StatusCode, Json<DeploymentRecord>)> {
    let conn = db_conn(&state)?;
    let record = db::create_deployment(&conn, req).map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn list_deployments(State(state): State<AppState>) -> ApiResult<Json<Vec<DeploymentRecord>>> {
    let conn = db_conn(&state)?;
    Ok(Json(db::list_deployments(&conn).map_err(map_db_error)?))
}

async fn get_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<DeploymentRecord>> {
    let conn = db_conn(&state)?;
    Ok(Json(db::get_deployment(&conn, &id).map_err(map_db_error)?))
}

async fn list_deployment_targets(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<DeploymentTargetRecord>>> {
    let conn = db_conn(&state)?;
    Ok(Json(
        db::list_deployment_targets(&conn, &id).map_err(map_db_error)?,
    ))
}

async fn pause_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<PauseDeploymentRequest>,
) -> ApiResult<Json<ApiMessage>> {
    let conn = db_conn(&state)?;
    db::set_deployment_paused(&conn, &id, req.paused).map_err(map_db_error)?;
    Ok(Json(ApiMessage {
        message: if req.paused {
            "paused".into()
        } else {
            "resumed".into()
        },
    }))
}

async fn abort_deployment(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<ApiMessage>> {
    let conn = db_conn(&state)?;
    db::abort_deployment(&conn, &id).map_err(map_db_error)?;
    Ok(Json(ApiMessage {
        message: "aborted".into(),
    }))
}

async fn agent_checkin(
    State(state): State<AppState>,
    Json(req): Json<AgentCheckinRequest>,
) -> ApiResult<Json<AssetRecord>> {
    let conn = db_conn(&state)?;
    Ok(Json(db::upsert_asset(&conn, req).map_err(map_db_error)?))
}

async fn agent_poll(
    State(state): State<AppState>,
    Json(req): Json<AgentPollRequest>,
) -> ApiResult<Json<AgentPollResponse>> {
    let conn = db_conn(&state)?;
    let commands = db::poll_commands(&conn, &req.asset_id).map_err(map_db_error)?;
    for cmd in &commands {
        db::mark_command_running(&conn, &cmd.id).map_err(map_db_error)?;
    }
    Ok(Json(AgentPollResponse { commands }))
}

async fn agent_result(
    State(state): State<AppState>,
    Json(req): Json<AgentResultRequest>,
) -> ApiResult<Json<ApiMessage>> {
    let conn = db_conn(&state)?;
    db::submit_command_result(&conn, req).map_err(map_db_error)?;
    Ok(Json(ApiMessage {
        message: "recorded".into(),
    }))
}

fn db_conn(state: &AppState) -> ApiResult<MutexGuard<'_, Connection>> {
    state.db.lock().map_err(|err| {
        ApiError::internal(format!("database mutex poisoned: {}", poison_message(err)))
    })
}

fn poison_message(err: PoisonError<MutexGuard<'_, Connection>>) -> String {
    err.to_string()
}

fn map_db_error(err: anyhow::Error) -> ApiError {
    let message = err.to_string();
    if message.contains("not found") {
        ApiError::not_found(message)
    } else if message.contains("mismatch")
        || message.contains("invalid")
        || message.contains("must not")
        || message.contains("requires")
        || message.contains("unknown ")
    {
        ApiError::invalid(message)
    } else {
        ApiError::internal(message)
    }
}
