//! Minimal Livy provenance HTTP contract owned by the resolver.

use crate::errors::ProvenanceError;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use reqwest::{
    Client, Response, Url,
    header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{fmt, time::Duration};

pub const DEFAULT_LIVY_API_BASE_URL: &str = "https://api.livylabs.xyz";
const MAX_PROVENANCE_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceVerificationMode {
    BindingOnly,
    Verify,
    VerifyFresh,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceCommitMode {
    Json,
    JsonSha256,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceFieldDisclosure {
    Public,
    Commitment,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceAttestationField {
    pub index: u32,
    pub name: String,
    pub commit_mode: ProvenanceCommitMode,
    pub value: Value,
    pub required: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CreateProvenanceAttestationRequest {
    pub attestation_claim: String,
    pub subject_type: String,
    pub subject_id: String,
    pub schema_id: Option<String>,
    pub schema_version: Option<String>,
    pub visibility: String,
    pub verification_mode: Option<ProvenanceVerificationMode>,
    pub attestation: Value,
    pub fields: Vec<ProvenanceAttestationField>,
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProvenanceAttestationResponse {
    pub provenance_attestation_id: String,
    pub subject_id: String,
    #[serde(default)]
    pub schema_id: Option<String>,
    #[serde(default)]
    pub schema_version: Option<String>,
    pub verification_status: String,
    pub schema_binding_status: String,
    pub public_values_commitment: String,
    pub report_payload_hash: String,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default)]
    pub registry_refs: Vec<ProvenanceRegistryRefResponse>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProvenanceRegistryRefResponse {
    pub registry_kind: String,
    pub provider: String,
    pub chain_family: String,
    #[serde(default)]
    pub chain_id: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub registry_name: Option<String>,
    #[serde(default)]
    pub registry_address: Option<String>,
    #[serde(default)]
    pub transaction_hash: Option<String>,
    #[serde(default)]
    pub block_number: Option<u64>,
    #[serde(default)]
    pub attestation_key: Option<String>,
    #[serde(default)]
    pub arweave_location: Option<String>,
    pub status: String,
    #[serde(default)]
    pub explorer_links: Value,
    #[serde(default)]
    pub registered_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceManagedPublicationRequest {
    arweave: bool,
    registry: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    livy_explorer_id: Option<String>,
    #[serde(default)]
    metadata: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<ProvenanceManagedPublicationArtifact>,
}

impl ProvenanceManagedPublicationRequest {
    pub fn livy_managed_registry() -> Self {
        Self {
            arweave: true,
            registry: true,
            livy_explorer_id: None,
            metadata: json!({}),
            artifacts: Vec::new(),
        }
    }

    pub fn with_livy_explorer_id(mut self, value: &str) -> Self {
        self.livy_explorer_id = Some(value.to_string());
        self
    }

    pub fn with_metadata(mut self, value: Value) -> Self {
        self.metadata = value;
        self
    }

    pub fn with_artifact(mut self, artifact: ProvenanceManagedPublicationArtifact) -> Self {
        self.artifacts.push(artifact);
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceManagedPublicationArtifact {
    name: String,
    content_type: String,
    data_base64: String,
    tags: Vec<ProvenanceArtifactTag>,
}

impl ProvenanceManagedPublicationArtifact {
    pub fn from_bytes(name: &str, content_type: &str, data: &[u8]) -> Self {
        Self {
            name: name.to_string(),
            content_type: content_type.to_string(),
            data_base64: BASE64_STANDARD.encode(data),
            tags: Vec::new(),
        }
    }

    pub fn with_tag(mut self, name: &str, value: &str) -> Self {
        self.tags.push(ProvenanceArtifactTag {
            name: name.to_string(),
            value: value.to_string(),
        });
        self
    }
}

#[derive(Clone, Debug, Serialize)]
struct ProvenanceArtifactTag {
    name: String,
    value: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProvenanceTemplateField {
    pub index: u32,
    pub name: String,
    pub commit_mode: ProvenanceCommitMode,
    pub disclosure: ProvenanceFieldDisclosure,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpsertProvenanceTemplateRequest {
    pub schema_id: String,
    pub schema_version: String,
    pub attestation_claim: String,
    pub subject_type: String,
    pub name: String,
    pub template_kind: String,
    pub description: String,
    pub visibility: String,
    pub fields: Vec<ProvenanceTemplateField>,
    pub metadata: Value,
}

pub struct ProvenanceHttpClient {
    base_url: String,
    api_key: String,
    integration_id: String,
    http: Client,
}

impl fmt::Debug for ProvenanceHttpClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProvenanceHttpClient")
            .field("base_url", &self.base_url)
            .field("api_key", &"[redacted]")
            .field("integration_id", &self.integration_id)
            .field("http", &self.http)
            .finish()
    }
}

impl ProvenanceHttpClient {
    pub fn new(
        base_url: String,
        api_key: String,
        integration_id: String,
        http: Client,
    ) -> Result<Self, ProvenanceError> {
        validate_base_url(&base_url)?;
        if api_key.trim().is_empty() {
            return Err(ProvenanceError::InvalidEnv(
                "LIVY_API_KEY must not be empty".to_string(),
            ));
        }
        if integration_id.trim().is_empty() {
            return Err(ProvenanceError::InvalidEnv(
                "LIVY_INTEGRATION_ID must not be empty".to_string(),
            ));
        }
        HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|_| {
            ProvenanceError::InvalidEnv("LIVY_API_KEY is not a valid header value".to_string())
        })?;
        HeaderValue::from_str(&integration_id).map_err(|_| {
            ProvenanceError::InvalidEnv(
                "LIVY_INTEGRATION_ID is not a valid header value".to_string(),
            )
        })?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            integration_id,
            http,
        })
    }

    pub async fn create_attestation(
        &self,
        request: CreateProvenanceAttestationRequest,
        publish: Option<ProvenanceManagedPublicationRequest>,
    ) -> Result<ProvenanceAttestationResponse, ProvenanceError> {
        let mut body = with_integration(request, &self.integration_id)?;
        if let Some(publish) = publish {
            body.as_object_mut()
                .ok_or_else(|| {
                    ProvenanceError::InvalidEnv(
                        "provenance request body must be an object".to_string(),
                    )
                })?
                .insert("publish".to_string(), serde_json::to_value(publish)?);
        }
        self.post_json("/api/v1/provenance/attestations", &body)
            .await
    }

    pub async fn upsert_template(
        &self,
        request: UpsertProvenanceTemplateRequest,
    ) -> Result<(), ProvenanceError> {
        let body = with_integration(request, &self.integration_id)?;
        let response = self
            .http
            .post(self.endpoint("/api/v1/provenance/templates"))
            .headers(self.headers()?)
            .json(&body)
            .send()
            .await?;
        ensure_success_response(response).await
    }

    pub async fn wait_for_registry_refs(
        &self,
        attestation_id: &str,
        attempts: u32,
        interval: Duration,
    ) -> Result<ProvenanceAttestationResponse, ProvenanceError> {
        if attempts == 0 {
            return Err(ProvenanceError::InvalidEnv(
                "registry wait attempts must be greater than zero".to_string(),
            ));
        }
        if !attestation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
        {
            return Err(ProvenanceError::InvalidEnv(
                "provenance attestation id contains unsafe path characters".to_string(),
            ));
        }

        for attempt in 0..attempts {
            let path = format!("/api/v1/public/provenance/attestations/{attestation_id}");
            let record: ProvenanceAttestationResponse = self.get_json(&path).await?;
            if !record.registry_refs.is_empty() {
                return Ok(record);
            }
            if attempt + 1 < attempts {
                tokio::time::sleep(interval).await;
            }
        }
        Err(ProvenanceError::RegistryWait(format!(
            "no registry reference appeared after {attempts} attempts"
        )))
    }

    async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, ProvenanceError> {
        let response = self
            .http
            .post(self.endpoint(path))
            .headers(self.headers()?)
            .json(body)
            .send()
            .await?;
        decode_json_response(response).await
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, ProvenanceError> {
        let response = self
            .http
            .get(self.endpoint(path))
            .headers(self.headers()?)
            .send()
            .await?;
        decode_json_response(response).await
    }

    fn endpoint(&self, path: &str) -> String {
        // A non-root base path is a reverse-proxy deployment prefix. Fixed v1
        // paths are appended beneath it; callers must not include `/api/v1` in
        // LIVY_BACKEND_BASE_URL.
        format!("{}{path}", self.base_url)
    }

    fn headers(&self) -> Result<HeaderMap, ProvenanceError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                .map_err(|err| ProvenanceError::InvalidEnv(err.to_string()))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-integration-id",
            HeaderValue::from_str(&self.integration_id)
                .map_err(|err| ProvenanceError::InvalidEnv(err.to_string()))?,
        );
        Ok(headers)
    }
}

pub fn build_http_client(timeout: Duration) -> Result<Client, ProvenanceError> {
    if timeout.is_zero() {
        return Err(ProvenanceError::InvalidEnv(
            "LIVY_PROVENANCE_TIMEOUT_SECS must be greater than zero".to_string(),
        ));
    }
    Client::builder()
        .connect_timeout(timeout.min(Duration::from_secs(5)))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(ProvenanceError::Http)
}

pub async fn decode_json_response<T: DeserializeOwned>(
    mut response: Response,
) -> Result<T, ProvenanceError> {
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > MAX_PROVENANCE_RESPONSE_BYTES {
            return Err(ProvenanceError::ResponseTooLarge {
                limit: MAX_PROVENANCE_RESPONSE_BYTES,
            });
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(ProvenanceError::Backend { status });
    }
    serde_json::from_slice(&body).map_err(ProvenanceError::Json)
}

async fn ensure_success_response(mut response: Response) -> Result<(), ProvenanceError> {
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > MAX_PROVENANCE_RESPONSE_BYTES {
            return Err(ProvenanceError::ResponseTooLarge {
                limit: MAX_PROVENANCE_RESPONSE_BYTES,
            });
        }
        body.extend_from_slice(&chunk);
    }
    if status.is_success() {
        Ok(())
    } else {
        Err(ProvenanceError::Backend { status })
    }
}

fn with_integration<T: Serialize>(
    request: T,
    integration_id: &str,
) -> Result<Value, ProvenanceError> {
    let mut body = serde_json::to_value(request)?;
    body.as_object_mut()
        .ok_or_else(|| {
            ProvenanceError::InvalidEnv("provenance request body must be an object".to_string())
        })?
        .insert("integration_id".to_string(), json!(integration_id));
    Ok(body)
}

pub fn validate_base_url(value: &str) -> Result<(), ProvenanceError> {
    let parsed = Url::parse(value).map_err(|_| {
        ProvenanceError::InvalidEnv("LIVY_BACKEND_BASE_URL must be an absolute URL".to_string())
    })?;
    let path_segments = parsed
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let includes_api_version = path_segments
        .windows(2)
        .any(|segments| segments == ["api", "v1"]);
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || includes_api_version
    {
        return Err(ProvenanceError::InvalidEnv(
            "LIVY_BACKEND_BASE_URL must be an http(s) origin or deployment prefix without credentials, /api/v1, query, or fragment".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::{Body, Bytes},
        extract::State,
        http::{HeaderMap, StatusCode, header::LOCATION},
        response::{IntoResponse, Response as AxumResponse},
        routing::{get, post},
    };
    use serde_json::json;
    use std::{
        convert::Infallible,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Instant,
    };
    use tokio::sync::{Mutex, mpsc};
    use tokio_stream::wrappers::ReceiverStream;

    const RESPONSE_V1_FIXTURE: &str =
        include_str!("../tests/fixtures/provenance_attestation_v1.json");

    #[derive(Clone, Debug)]
    struct CapturedRequest {
        operation: &'static str,
        headers: HeaderMap,
        body: Value,
    }

    #[derive(Clone, Default)]
    struct CaptureState(Arc<Mutex<Vec<CapturedRequest>>>);

    fn sample_attestation_request() -> CreateProvenanceAttestationRequest {
        CreateProvenanceAttestationRequest {
            attestation_claim: "source".to_string(),
            subject_type: "resolver_fetch".to_string(),
            subject_id: "resolver_fetch:subject".to_string(),
            schema_id: Some("resolver-fetch-v1".to_string()),
            schema_version: Some("1".to_string()),
            visibility: "private".to_string(),
            verification_mode: Some(ProvenanceVerificationMode::VerifyFresh),
            attestation: json!({"proof": "test"}),
            fields: vec![ProvenanceAttestationField {
                index: 0,
                name: "source".to_string(),
                commit_mode: ProvenanceCommitMode::JsonSha256,
                value: json!({"url": "https://example.com"}),
                required: true,
            }],
            metadata: json!({"origin": "resolver"}),
        }
    }

    fn sample_publication_request() -> ProvenanceManagedPublicationRequest {
        ProvenanceManagedPublicationRequest::livy_managed_registry()
            .with_livy_explorer_id("resolver_fetch:subject")
            .with_metadata(json!({"route": "extract"}))
            .with_artifact(
                ProvenanceManagedPublicationArtifact::from_bytes(
                    "resolver-response.json",
                    "application/json",
                    br#"{"ok":true}"#,
                )
                .with_tag("Livy-Artifact-Kind", "resolver-response")
                .with_tag("Livy-Artifact-Schema", "resolver-tool-exchange-v1"),
            )
    }

    fn sample_template_request() -> UpsertProvenanceTemplateRequest {
        UpsertProvenanceTemplateRequest {
            schema_id: "resolver-fetch-v1".to_string(),
            schema_version: "1".to_string(),
            attestation_claim: "source".to_string(),
            subject_type: "resolver_fetch".to_string(),
            name: "Resolver fetch".to_string(),
            template_kind: "resolver_source".to_string(),
            description: "Generic resolver source-fetch proof".to_string(),
            visibility: "private".to_string(),
            fields: vec![ProvenanceTemplateField {
                index: 0,
                name: "source".to_string(),
                commit_mode: ProvenanceCommitMode::JsonSha256,
                disclosure: ProvenanceFieldDisclosure::Commitment,
                required: true,
                value_type: Some("json".to_string()),
                description: Some("Committed source".to_string()),
            }],
            metadata: json!({"service": "livy-resolver"}),
        }
    }

    fn test_client(base_url: String, timeout: Duration) -> ProvenanceHttpClient {
        ProvenanceHttpClient::new(
            base_url,
            "service-key".to_string(),
            "resolver-test".to_string(),
            build_http_client(timeout).unwrap(),
        )
        .unwrap()
    }

    async fn serve(app: Router) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        address
    }

    fn assert_contract_headers(headers: &HeaderMap) {
        assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Bearer service-key");
        assert_eq!(headers.get(CONTENT_TYPE).unwrap(), "application/json");
        assert_eq!(headers.get("x-integration-id").unwrap(), "resolver-test");
    }

    #[test]
    fn managed_publication_artifact_uses_standard_base64_and_ordered_tags() {
        let value = serde_json::to_value(
            ProvenanceManagedPublicationRequest::livy_managed_registry()
                .with_livy_explorer_id("subject")
                .with_artifact(
                    ProvenanceManagedPublicationArtifact::from_bytes(
                        "response.json",
                        "application/json",
                        br#"{"ok":true}"#,
                    )
                    .with_tag("kind", "resolver")
                    .with_tag("schema", "v1"),
                ),
        )
        .unwrap();
        assert_eq!(value["arweave"], true);
        assert_eq!(value["registry"], true);
        assert_eq!(value["artifacts"][0]["data_base64"], "eyJvayI6dHJ1ZX0=");
        assert_eq!(value["artifacts"][0]["tags"][0]["name"], "kind");
        assert_eq!(value["artifacts"][0]["tags"][1]["name"], "schema");
    }

    #[tokio::test]
    async fn non_success_response_is_rejected() {
        let app = Router::new().route(
            "/error",
            post(|| async { (StatusCode::BAD_GATEWAY, Json(json!({"error": "upstream"}))) }),
        );
        let address = serve(app).await;
        let response = Client::new()
            .post(format!("http://{address}/error"))
            .send()
            .await
            .unwrap();
        assert!(matches!(
            decode_json_response::<Value>(response).await,
            Err(ProvenanceError::Backend {
                status: StatusCode::BAD_GATEWAY
            })
        ));
    }

    async fn chunked_oversized_body() -> AxumResponse {
        let (sender, receiver) = mpsc::channel::<Result<Bytes, Infallible>>(8);
        tokio::spawn(async move {
            for _ in 0..5 {
                let chunk = Bytes::from(vec![b'x'; MAX_PROVENANCE_RESPONSE_BYTES / 4]);
                if sender.send(Ok(chunk)).await.is_err() {
                    return;
                }
            }
        });
        AxumResponse::new(Body::from_stream(ReceiverStream::new(receiver)))
    }

    #[tokio::test]
    async fn chunked_response_over_one_mebibyte_is_rejected() {
        let address = serve(Router::new().route("/large", get(chunked_oversized_body))).await;
        let response = Client::new()
            .get(format!("http://{address}/large"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.content_length(), None);
        assert!(matches!(
            decode_json_response::<Value>(response).await,
            Err(ProvenanceError::ResponseTooLarge {
                limit: MAX_PROVENANCE_RESPONSE_BYTES
            })
        ));
    }

    #[tokio::test]
    async fn absolute_timeout_covers_a_stalled_response_body() {
        async fn stalled_body() -> AxumResponse {
            let (sender, receiver) = mpsc::channel::<Result<Bytes, Infallible>>(2);
            sender.send(Ok(Bytes::from_static(b"{"))).await.unwrap();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let _ = sender.send(Ok(Bytes::from_static(b"\"ok\":true}"))).await;
            });
            AxumResponse::new(Body::from_stream(ReceiverStream::new(receiver)))
        }

        let address = serve(Router::new().route("/stall", get(stalled_body))).await;
        let http = build_http_client(Duration::from_millis(150)).unwrap();
        let started = Instant::now();
        let response = http
            .get(format!("http://{address}/stall"))
            .send()
            .await
            .unwrap();
        match decode_json_response::<Value>(response).await {
            Err(ProvenanceError::Http(error)) => assert!(error.is_timeout()),
            other => panic!("expected a body timeout, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let target_hits = Arc::new(AtomicUsize::new(0));
        let target_state = Arc::clone(&target_hits);
        let app = Router::new()
            .route(
                "/api/v1/provenance/attestations",
                post(|| async {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [(LOCATION, "/redirect-target")],
                    )
                }),
            )
            .route(
                "/redirect-target",
                post(move || {
                    let target_state = Arc::clone(&target_state);
                    async move {
                        target_state.fetch_add(1, Ordering::SeqCst);
                        (StatusCode::OK, RESPONSE_V1_FIXTURE)
                    }
                }),
            );
        let address = serve(app).await;
        let error = test_client(format!("http://{address}"), Duration::from_secs(1))
            .create_attestation(sample_attestation_request(), None)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ProvenanceError::Backend {
                status: StatusCode::TEMPORARY_REDIRECT
            }
        ));
        assert_eq!(target_hits.load(Ordering::SeqCst), 0);
    }

    async fn capture_attestation(
        State(state): State<CaptureState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> impl IntoResponse {
        state.0.lock().await.push(CapturedRequest {
            operation: "attestation",
            headers,
            body,
        });
        (
            StatusCode::CREATED,
            [(CONTENT_TYPE, "application/json")],
            RESPONSE_V1_FIXTURE,
        )
    }

    async fn capture_template(
        State(state): State<CaptureState>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> StatusCode {
        state.0.lock().await.push(CapturedRequest {
            operation: "template",
            headers,
            body,
        });
        StatusCode::NO_CONTENT
    }

    #[tokio::test]
    async fn exact_v1_requests_headers_and_base_path_are_preserved() {
        let state = CaptureState::default();
        let app = Router::new()
            .route(
                "/deployment/livy/api/v1/provenance/attestations",
                post(capture_attestation),
            )
            .route(
                "/deployment/livy/api/v1/provenance/templates",
                post(capture_template),
            )
            .with_state(state.clone());
        let address = serve(app).await;
        let client = test_client(
            format!("http://{address}/deployment/livy///"),
            Duration::from_secs(1),
        );

        let response = client
            .create_attestation(
                sample_attestation_request(),
                Some(sample_publication_request()),
            )
            .await
            .unwrap();
        assert_eq!(response.provenance_attestation_id, "attestation-v1-fixture");
        client
            .upsert_template(sample_template_request())
            .await
            .unwrap();

        let captured = state.0.lock().await;
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].operation, "attestation");
        assert_contract_headers(&captured[0].headers);
        assert_eq!(
            captured[0].body,
            json!({
                "attestation_claim": "source",
                "subject_type": "resolver_fetch",
                "subject_id": "resolver_fetch:subject",
                "schema_id": "resolver-fetch-v1",
                "schema_version": "1",
                "visibility": "private",
                "verification_mode": "verify_fresh",
                "attestation": {"proof": "test"},
                "fields": [{
                    "index": 0,
                    "name": "source",
                    "commit_mode": "json_sha256",
                    "value": {"url": "https://example.com"},
                    "required": true
                }],
                "metadata": {"origin": "resolver"},
                "integration_id": "resolver-test",
                "publish": {
                    "arweave": true,
                    "registry": true,
                    "livy_explorer_id": "resolver_fetch:subject",
                    "metadata": {"route": "extract"},
                    "artifacts": [{
                        "name": "resolver-response.json",
                        "content_type": "application/json",
                        "data_base64": "eyJvayI6dHJ1ZX0=",
                        "tags": [
                            {"name": "Livy-Artifact-Kind", "value": "resolver-response"},
                            {"name": "Livy-Artifact-Schema", "value": "resolver-tool-exchange-v1"}
                        ]
                    }]
                }
            })
        );

        assert_eq!(captured[1].operation, "template");
        assert_contract_headers(&captured[1].headers);
        assert_eq!(
            captured[1].body,
            json!({
                "schema_id": "resolver-fetch-v1",
                "schema_version": "1",
                "attestation_claim": "source",
                "subject_type": "resolver_fetch",
                "name": "Resolver fetch",
                "template_kind": "resolver_source",
                "description": "Generic resolver source-fetch proof",
                "visibility": "private",
                "fields": [{
                    "index": 0,
                    "name": "source",
                    "commit_mode": "json_sha256",
                    "disclosure": "commitment",
                    "required": true,
                    "value_type": "json",
                    "description": "Committed source"
                }],
                "metadata": {"service": "livy-resolver"},
                "integration_id": "resolver-test"
            })
        );
    }

    #[test]
    fn current_v1_response_fixture_parses() {
        let response: ProvenanceAttestationResponse =
            serde_json::from_str(RESPONSE_V1_FIXTURE).unwrap();
        assert_eq!(response.provenance_attestation_id, "attestation-v1-fixture");
        assert_eq!(response.schema_id.as_deref(), Some("resolver-fetch-v1"));
        assert_eq!(response.schema_version.as_deref(), Some("1"));
        assert_eq!(response.registry_refs.len(), 1);
        assert_eq!(response.registry_refs[0].registry_kind, "evm");
        assert_eq!(response.registry_refs[0].block_number, Some(12_345));
        assert_eq!(
            response.registry_refs[0].explorer_links["transaction"],
            "https://explorer.example/tx/0xabc"
        );
    }

    #[test]
    fn base_url_accepts_a_reverse_proxy_prefix_but_rejects_url_components() {
        assert!(validate_base_url("https://api.example/deployment/livy/").is_ok());
        assert!(validate_base_url("https://user@api.example").is_err());
        assert!(validate_base_url("https://api.example/deployment/api/v1").is_err());
        assert!(validate_base_url("https://api.example?tenant=1").is_err());
        assert!(validate_base_url("https://api.example#fragment").is_err());
    }

    #[test]
    fn debug_output_redacts_the_service_key() {
        let client = test_client(
            "https://api.example/deployment/livy".to_string(),
            Duration::from_secs(1),
        );
        let debug = format!("{client:?}");
        assert!(!debug.contains("service-key"));
        assert!(debug.contains("[redacted]"));
    }
}
