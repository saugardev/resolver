//! Liveness, readiness, and graceful-drain state.

use axum::{
    Json,
    extract::Extension,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::egress::EgressPolicy;

#[derive(Debug)]
pub struct RuntimeState {
    accepting_requests: AtomicBool,
}

impl RuntimeState {
    pub fn new() -> Self {
        Self {
            accepting_requests: AtomicBool::new(false),
        }
    }

    pub fn mark_ready(&self) {
        self.accepting_requests.store(true, Ordering::Release);
    }

    pub fn begin_draining(&self) {
        self.accepting_requests.store(false, Ordering::Release);
    }

    fn is_accepting_requests(&self) -> bool {
        self.accepting_requests.load(Ordering::Acquire)
    }
}

pub async fn liveness() -> Json<serde_json::Value> {
    Json(json!({ "status": "alive" }))
}

pub async fn readiness(
    Extension(state): Extension<Arc<RuntimeState>>,
    Extension(egress): Extension<Arc<EgressPolicy>>,
) -> Response {
    let accepting_requests = state.is_accepting_requests();
    let dependency_ready = if accepting_requests {
        egress.require_actual_fetch_capability().await.is_ok()
    } else {
        false
    };
    if accepting_requests && dependency_ready {
        (
            StatusCode::OK,
            Json(json!({
                "status": "ready",
                "dependencies": "ready",
                "actual_fetch_egress": "ready",
                "accepting_requests": true,
            })),
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "not_ready",
                "dependencies": "unavailable",
                "actual_fetch_egress": if dependency_ready { "ready" } else { "unavailable" },
                "accepting_requests": accepting_requests,
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
        let state = Arc::new(RuntimeState::new());
        let egress = Arc::new(EgressPolicy::for_tests(&[], true));
        assert_eq!(
            readiness(Extension(state.clone()), Extension(egress.clone()))
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        state.mark_ready();
        assert_eq!(
            readiness(Extension(state.clone()), Extension(egress.clone()))
                .await
                .status(),
            StatusCode::OK
        );

        state.begin_draining();
        assert_eq!(
            readiness(Extension(state), Extension(egress))
                .await
                .status(),
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
        let state = Arc::new(RuntimeState::new());
        let egress = Arc::new(EgressPolicy::for_tests(&[], false));
        state.mark_ready();
        assert_eq!(
            readiness(Extension(state), Extension(egress))
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
