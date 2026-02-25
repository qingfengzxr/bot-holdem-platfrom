use axum::{Json, Router, extract::Path, routing::get};
use serde::Serialize;
use tracing::info;

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub service: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsResponse {
    pub format: &'static str,
    pub body: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminStubResponse {
    pub ok: bool,
    pub resource: String,
    pub id: String,
    pub note: &'static str,
}

pub fn build_router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/admin/rooms/:id", get(admin_room))
        .route("/admin/hands/:id/replay", get(admin_hand_replay))
        .route("/admin/audit/request/:id", get(admin_audit_request))
        .route("/admin/audit/tx/:id", get(admin_audit_tx))
}

async fn health() -> Json<HealthResponse> {
    info!(route = "/health", "ops http request");
    Json(HealthResponse {
        ok: true,
        service: "ops-http",
    })
}

async fn metrics() -> Json<MetricsResponse> {
    info!(route = "/metrics", "ops http request");
    Json(MetricsResponse {
        format: "prometheus-text",
        body: "# metrics not implemented yet\n",
    })
}

async fn admin_room(Path(id): Path<String>) -> Json<AdminStubResponse> {
    info!(route = "/admin/rooms/:id", room_id = %id, "ops http request");
    Json(AdminStubResponse {
        ok: true,
        resource: "room".to_string(),
        id,
        note: "admin room view stub",
    })
}

async fn admin_hand_replay(Path(id): Path<String>) -> Json<AdminStubResponse> {
    info!(route = "/admin/hands/:id/replay", hand_id = %id, "ops http request");
    Json(AdminStubResponse {
        ok: true,
        resource: "hand_replay".to_string(),
        id,
        note: "admin hand replay stub",
    })
}

async fn admin_audit_request(Path(id): Path<String>) -> Json<AdminStubResponse> {
    info!(route = "/admin/audit/request/:id", request_id = %id, "ops http request");
    Json(AdminStubResponse {
        ok: true,
        resource: "audit_request".to_string(),
        id,
        note: "admin audit request stub",
    })
}

async fn admin_audit_tx(Path(id): Path<String>) -> Json<AdminStubResponse> {
    info!(route = "/admin/audit/tx/:id", tx_hash = %id, "ops http request");
    Json(AdminStubResponse {
        ok: true,
        resource: "audit_tx".to_string(),
        id,
        note: "admin audit tx stub",
    })
}
