//! Liveness, readiness, and graceful-drain state.

use axum::{
    Json,
    extract::Extension,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub struct RuntimeState {
    dependencies_ready: bool,
    accepting_requests: AtomicBool,
}

impl RuntimeState {
    pub fn new(dependencies_ready: bool) -> Self {
        Self {
            dependencies_ready,
            accepting_requests: AtomicBool::new(false),
        }
    }

    pub fn mark_ready(&self) {
        if self.dependencies_ready {
            self.accepting_requests.store(true, Ordering::Release);
        }
    }

    pub fn begin_draining(&self) {
        self.accepting_requests.store(false, Ordering::Release);
    }

    fn is_ready(&self) -> bool {
        self.dependencies_ready && self.accepting_requests.load(Ordering::Acquire)
    }
}

pub async fn liveness() -> Json<serde_json::Value> {
    Json(json!({ "status": "alive" }))
}

pub async fn readiness(Extension(state): Extension<std::sync::Arc<RuntimeState>>) -> Response {
    if state.is_ready() {
        (
            StatusCode::OK,
            Json(json!({
                "status": "ready",
                "dependencies": "configured",
                "accepting_requests": true,
            })),
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "not_ready",
                "dependencies": if state.dependencies_ready { "configured" } else { "unavailable" },
                "accepting_requests": false,
            })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use std::sync::Arc;

    #[tokio::test]
    async fn readiness_tracks_startup_and_drain_without_affecting_liveness() {
        let state = Arc::new(RuntimeState::new(true));
        assert_eq!(
            readiness(Extension(state.clone())).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        state.mark_ready();
        assert_eq!(
            readiness(Extension(state.clone())).await.status(),
            StatusCode::OK
        );

        state.begin_draining();
        assert_eq!(
            readiness(Extension(state)).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        let body = to_bytes(liveness().await.into_response().into_body(), usize::MAX)
            .await
            .expect("liveness body");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).expect("liveness JSON")["status"],
            "alive"
        );
    }

    #[tokio::test]
    async fn dependency_failure_cannot_be_marked_ready() {
        let state = Arc::new(RuntimeState::new(false));
        state.mark_ready();
        assert_eq!(
            readiness(Extension(state)).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
