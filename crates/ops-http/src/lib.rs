use axum::{Json, Router, extract::Path, routing::get};
use serde::Serialize;

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
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        service: "ops-http",
    })
}

async fn metrics() -> Json<MetricsResponse> {
    Json(MetricsResponse {
        format: "prometheus-text",
        body: "# metrics not implemented yet\n",
    })
}

async fn admin_room(Path(id): Path<String>) -> Json<AdminStubResponse> {
    Json(AdminStubResponse {
        ok: true,
        resource: "room".to_string(),
        id,
        note: "admin room view stub",
    })
}

async fn admin_hand_replay(Path(id): Path<String>) -> Json<AdminStubResponse> {
    Json(AdminStubResponse {
        ok: true,
        resource: "hand_replay".to_string(),
        id,
        note: "admin hand replay stub",
    })
}
