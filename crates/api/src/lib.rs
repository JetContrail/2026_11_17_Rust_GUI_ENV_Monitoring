//! External-integration REST API (HTTPS only).
//!
//! | Method | Path | Role |
//! |---|---|---|
//! | GET | `/api/v1/health` | none |
//! | GET | `/api/v1/users/{id}` | viewer |
//! | PUT | `/api/v1/users/{id}` | admin |
//! | GET | `/api/v1/doors` | viewer |
//! | POST | `/api/v1/doors/{id}/unlock` | operator |
//! | POST | `/api/v1/doors/{id}/acknowledge` | operator |
//! | GET | `/api/v1/alarms?active=true` | viewer |
//! | GET | `/api/v1/history?limit=100` | viewer |
//! | GET | `/api/v1/objects?since=0` | viewer |
//!
//! Requests authenticate with `Authorization: Bearer <token>` (see [`auth`]).

pub mod auth;
pub mod server;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use envmon_core::door::DoorIndication;
use envmon_core::model::{DoorId, User, UserId};
use envmon_core::objects::ObjectEntry;
use envmon_core::system::{AccessRecord, Alarm, System, SystemError};
use envmon_protocol::host::Clock;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub use auth::{Principal, Role, TokenStore};

/// Maximum records returned by `/history`.
pub const HISTORY_LIMIT: usize = 1_000;

#[derive(Clone)]
pub struct ApiState {
    system: Arc<Mutex<System>>,
    tokens: Arc<TokenStore>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ApiState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiState").finish_non_exhaustive()
    }
}

impl ApiState {
    pub fn new(system: Arc<Mutex<System>>, tokens: TokenStore, clock: Arc<dyn Clock>) -> Self {
        Self {
            system,
            tokens: Arc::new(tokens),
            clock,
        }
    }

    fn system(&self) -> MutexGuard<'_, System> {
        self.system.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("missing or invalid bearer token")]
    Unauthorized,
    #[error("requires role {0:?}")]
    Forbidden(Role),
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<SystemError> for ApiError {
    fn from(e: SystemError) -> Self {
        match e {
            SystemError::UnknownDoor(d) => Self::NotFound(format!("door {d}")),
            SystemError::Directory(e) => Self::BadRequest(e.to_string()),
            e => Self::Internal(e.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = Json(json!({ "error": self.to_string() }));
        if status == StatusCode::UNAUTHORIZED {
            (status, [(header::WWW_AUTHENTICATE, "Bearer")], body).into_response()
        } else {
            (status, body).into_response()
        }
    }
}

impl Principal {
    pub fn require(&self, role: Role) -> Result<(), ApiError> {
        if self.role >= role {
            Ok(())
        } else {
            Err(ApiError::Forbidden(role))
        }
    }
}

impl FromRequestParts<ApiState> for Principal {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &ApiState) -> Result<Self, ApiError> {
        parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .and_then(|token| state.tokens.authenticate(token.trim()))
            .ok_or(ApiError::Unauthorized)
    }
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/users/{id}", get(get_user).put(put_user))
        .route("/api/v1/doors", get(list_doors))
        .route("/api/v1/doors/{id}/unlock", post(unlock_door))
        .route("/api/v1/doors/{id}/acknowledge", post(acknowledge))
        .route("/api/v1/alarms", get(alarms))
        .route("/api/v1/history", get(history))
        .route("/api/v1/objects", get(objects))
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn get_user(
    p: Principal,
    State(s): State<ApiState>,
    Path(id): Path<u32>,
) -> Result<Json<User>, ApiError> {
    p.require(Role::Viewer)?;
    s.system()
        .directory()
        .user(UserId(id))
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("user {id}")))
}

async fn put_user(
    p: Principal,
    State(s): State<ApiState>,
    Path(id): Path<u32>,
    Json(user): Json<User>,
) -> Result<StatusCode, ApiError> {
    p.require(Role::Admin)?;
    if user.id != UserId(id) {
        return Err(ApiError::BadRequest(format!(
            "body id {} does not match path id {id}",
            user.id
        )));
    }
    s.system().upsert_user(user)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct DoorStatus {
    pub id: DoorId,
    pub name: String,
    #[serde(flatten)]
    pub indication: DoorIndication,
}

async fn list_doors(
    p: Principal,
    State(s): State<ApiState>,
) -> Result<Json<Vec<DoorStatus>>, ApiError> {
    p.require(Role::Viewer)?;
    let sys = s.system();
    let mut doors: Vec<DoorStatus> = sys
        .door_configs()
        .filter_map(|c| {
            sys.door_indication(c.id).map(|indication| DoorStatus {
                id: c.id,
                name: c.name.clone(),
                indication,
            })
        })
        .collect();
    doors.sort_by_key(|d| d.id);
    Ok(Json(doors))
}

async fn unlock_door(
    p: Principal,
    State(s): State<ApiState>,
    Path(id): Path<u32>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    p.require(Role::Operator)?;
    let (at, now) = s.clock.now();
    s.system().remote_unlock(DoorId(id), at, now)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "door": id, "unlocked_by": p.name })),
    ))
}

async fn acknowledge(
    p: Principal,
    State(s): State<ApiState>,
    Path(id): Path<u32>,
) -> Result<Json<serde_json::Value>, ApiError> {
    p.require(Role::Operator)?;
    let (at, _) = s.clock.now();
    let acknowledged = s.system().acknowledge(DoorId(id), at)?;
    Ok(Json(json!({ "door": id, "acknowledged": acknowledged })))
}

#[derive(Debug, Deserialize)]
struct AlarmQuery {
    #[serde(default)]
    active: bool,
}

async fn alarms(
    p: Principal,
    State(s): State<ApiState>,
    Query(q): Query<AlarmQuery>,
) -> Result<Json<Vec<Alarm>>, ApiError> {
    p.require(Role::Viewer)?;
    let sys = s.system();
    Ok(Json(
        sys.alarms()
            .filter(|a| !q.active || a.is_active())
            .cloned()
            .collect(),
    ))
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    limit: Option<usize>,
}

/// Newest first.
async fn history(
    p: Principal,
    State(s): State<ApiState>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<AccessRecord>>, ApiError> {
    p.require(Role::Viewer)?;
    let limit = q.limit.unwrap_or(100).min(HISTORY_LIMIT);
    Ok(Json(
        s.system().history().rev().take(limit).cloned().collect(),
    ))
}

#[derive(Debug, Deserialize)]
struct ObjectsQuery {
    #[serde(default)]
    since: u64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ObjectsResponse {
    /// Pass as `since` next time to get only changed objects.
    pub version: u64,
    pub objects: Vec<ObjectEntry>,
}

async fn objects(
    p: Principal,
    State(s): State<ApiState>,
    Query(q): Query<ObjectsQuery>,
) -> Result<Json<ObjectsResponse>, ApiError> {
    p.require(Role::Viewer)?;
    let sys = s.system();
    Ok(Json(ObjectsResponse {
        version: sys.objects().version(),
        objects: sys.objects().changed_since(q.since).cloned().collect(),
    }))
}
