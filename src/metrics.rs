//! Low-cardinality process metrics for the resolver HTTP surface.

use axum::{
    body::Body,
    extract::{Extension, State},
    http::{Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

/// Process-local counters. A metrics collector should scrape `/metrics` and
/// aggregate across replicas; no URL, tenant, token, or request identifier is
/// used as a label.
#[derive(Debug, Default)]
pub struct RuntimeMetrics {
    requests_total: AtomicU64,
    requests_in_flight: AtomicU64,
    responses_4xx_total: AtomicU64,
    responses_5xx_total: AtomicU64,
    request_duration_ms_total: AtomicU64,
}

impl RuntimeMetrics {
    fn observe(&self, status: StatusCode, elapsed_ms: u64) {
        if status.is_client_error() {
            self.responses_4xx_total.fetch_add(1, Ordering::Relaxed);
        } else if status.is_server_error() {
            self.responses_5xx_total.fetch_add(1, Ordering::Relaxed);
        }
        self.request_duration_ms_total
            .fetch_add(elapsed_ms, Ordering::Relaxed);
    }

    fn render(&self) -> String {
        format!(
            concat!(
                "# HELP livy_resolver_http_requests_total HTTP requests handled by this process.\n",
                "# TYPE livy_resolver_http_requests_total counter\n",
                "livy_resolver_http_requests_total {}\n",
                "# HELP livy_resolver_http_requests_in_flight HTTP requests currently executing.\n",
                "# TYPE livy_resolver_http_requests_in_flight gauge\n",
                "livy_resolver_http_requests_in_flight {}\n",
                "# HELP livy_resolver_http_responses_4xx_total HTTP client-error responses.\n",
                "# TYPE livy_resolver_http_responses_4xx_total counter\n",
                "livy_resolver_http_responses_4xx_total {}\n",
                "# HELP livy_resolver_http_responses_5xx_total HTTP server-error responses.\n",
                "# TYPE livy_resolver_http_responses_5xx_total counter\n",
                "livy_resolver_http_responses_5xx_total {}\n",
                "# HELP livy_resolver_http_request_duration_ms_total Cumulative request duration in milliseconds.\n",
                "# TYPE livy_resolver_http_request_duration_ms_total counter\n",
                "livy_resolver_http_request_duration_ms_total {}\n"
            ),
            self.requests_total.load(Ordering::Relaxed),
            self.requests_in_flight.load(Ordering::Relaxed),
            self.responses_4xx_total.load(Ordering::Relaxed),
            self.responses_5xx_total.load(Ordering::Relaxed),
            self.request_duration_ms_total.load(Ordering::Relaxed),
        )
    }
}

struct InFlightGuard<'a>(&'a AtomicU64);

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub async fn record_http_metrics(
    State(metrics): State<Arc<RuntimeMetrics>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    metrics.requests_total.fetch_add(1, Ordering::Relaxed);
    metrics.requests_in_flight.fetch_add(1, Ordering::Relaxed);
    let in_flight = InFlightGuard(&metrics.requests_in_flight);
    let started = Instant::now();
    let response = next.run(request).await;
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    metrics.observe(response.status(), elapsed_ms);
    drop(in_flight);
    response
}

pub async fn prometheus(Extension(metrics): Extension<Arc<RuntimeMetrics>>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics.render(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use tower::ServiceExt;

    #[tokio::test]
    async fn concurrent_requests_are_counted_without_sensitive_labels() {
        let metrics = Arc::new(RuntimeMetrics::default());
        let app = Router::new()
            .route("/ok", get(|| async { StatusCode::OK }))
            .route(
                "/failure",
                get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            )
            .layer(axum::middleware::from_fn_with_state(
                metrics.clone(),
                record_http_metrics,
            ));

        let (ok, failure) = tokio::join!(
            app.clone()
                .oneshot(Request::builder().uri("/ok").body(Body::empty()).unwrap()),
            app.oneshot(
                Request::builder()
                    .uri("/failure")
                    .body(Body::empty())
                    .unwrap()
            )
        );
        assert_eq!(ok.unwrap().status(), StatusCode::OK);
        assert_eq!(failure.unwrap().status(), StatusCode::INTERNAL_SERVER_ERROR);

        let rendered = metrics.render();
        assert!(rendered.contains("livy_resolver_http_requests_total 2"));
        assert!(rendered.contains("livy_resolver_http_requests_in_flight 0"));
        assert!(rendered.contains("livy_resolver_http_responses_5xx_total 1"));
        assert!(!rendered.contains("/ok"));
        assert!(!rendered.contains("/failure"));
    }
}
