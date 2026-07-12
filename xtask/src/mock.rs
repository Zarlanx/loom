// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Dev-only mock server for CLI development (PR-04b).
//!
//! Answers every Phase-1 golden-path route from `openapi.json` with
//! contract-shaped fixtures so the `loom` CLI (PR-10) can be built and demoed
//! before any real handler exists. It lives in `xtask` — dev tooling that never
//! ships in a release binary (workspace-setup.md §1) — and is exercised in-process
//! by the test below. The route set is `crate::openapi::GOLDEN_PATH_ROUTES`, the same
//! single source of truth the spec validator checks against.

// axum handlers must be `async` and take extractors by value; both are inherent to
// the framework rather than avoidable here, so relax the two pedantic lints they trip.
#![allow(clippy::unused_async, clippy::needless_pass_by_value)]

use std::net::SocketAddr;

use anyhow::{Context, Result};
use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::openapi::GOLDEN_PATH_ROUTES;

/// Build the mock router: one handler per golden-path route.
pub(crate) fn app() -> Router {
    Router::new()
        .route("/v1/account", get(get_account))
        .route("/v1/jobs", post(create_job).get(list_jobs))
        .route("/v1/jobs/{id}", get(get_job))
        .route("/v1/jobs/{id}/logs", get(stream_job_logs))
        .route("/v1/jobs/{id}/cancel", post(cancel_job))
        .route("/v1/datasets", post(create_dataset))
        .route("/v1/datasets/{id}/versions", post(begin_dataset_version))
        .route(
            "/v1/datasets/{id}/versions/{vid}/commit",
            post(commit_dataset_version),
        )
        .route("/v1/deployments", post(create_deployment))
        .route("/v1/deployments/{id}", get(get_deployment))
}

/// Bind `addr` and serve the mock until the process is killed.
pub(crate) async fn serve(addr: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    let actual = listener.local_addr().context("resolving local addr")?;
    println!(
        "xtask mock-server: listening on http://{actual} — {} golden-path routes (dev only)",
        GOLDEN_PATH_ROUTES.len()
    );
    axum::serve(listener, app()).await.context("serving mock")
}

// --- Handlers: contract-shaped fixtures (renter-api.md §3, echoed by openapi.json) ---

async fn get_account() -> Json<Value> {
    Json(json!({
        "id": "acct_local",
        "profile": "standalone",
        "key": { "id": "key_local_admin", "name": "local-admin", "scopes": ["admin"] }
    }))
}

async fn create_job() -> impl IntoResponse {
    let body = json!({
        "id": "grp_mock",
        "kind": "job_group",
        "children": [ { "id": "job_mock_a", "gpu": "rtx4090", "state": "queued" } ],
        "estimate": {
            "gpu_hours_low": 1.2, "gpu_hours_high": 2.4,
            "cost_micro_usd_low": 410_000, "cost_micro_usd_high": 820_000
        }
    });
    (StatusCode::CREATED, Json(body))
}

async fn list_jobs() -> Json<Value> {
    Json(json!({
        "data": [ { "id": "job_mock_a", "state": "running", "gpu": "rtx4090" } ],
        "next_cursor": null,
        "has_more": false
    }))
}

async fn get_job(Path(id): Path<String>) -> Json<Value> {
    Json(json!({
        "id": id,
        "group_id": "grp_mock",
        "state": "running",
        "gpu": "rtx4090",
        "isolation_tier": "B",
        "cost": { "accrued_micro_usd": 21000, "held_micro_usd": 610_000, "billable_seconds": 47 },
        "attempts": [
            { "attempt_no": 1, "state": "running", "started_at": "2026-07-11T00:00:00Z" }
        ],
        "created_at": "2026-07-11T00:00:00Z"
    }))
}

async fn stream_job_logs(Path(id): Path<String>) -> impl IntoResponse {
    // Build the SSE `data:` JSON with serde so a job id containing `"` or `\`
    // (e.g. via a URL-encoded path) is escaped instead of producing malformed JSON.
    let log_data = serde_json::to_string(&json!({
        "attempt_no": 1,
        "stream": "stdout",
        "line": format!("hello from {id}"),
    }))
    .expect("infallible json");
    let body = format!(
        "id: 1\nevent: log\ndata: {log_data}\n\n\
         id: 2\nevent: done\ndata: {{\"state\":\"succeeded\"}}\n\n"
    );
    ([(header::CONTENT_TYPE, "text/event-stream")], body)
}

async fn cancel_job(Path(id): Path<String>) -> impl IntoResponse {
    let body = json!({ "id": id, "state": "cancelled", "gpu": "rtx4090" });
    (StatusCode::ACCEPTED, Json(body))
}

async fn create_dataset() -> impl IntoResponse {
    let body = json!({ "id": "ds_mock", "name": "my-sft", "created_at": "2026-07-11T00:00:00Z" });
    (StatusCode::CREATED, Json(body))
}

async fn begin_dataset_version() -> impl IntoResponse {
    let body = json!({
        "version_id": "dsv_mock",
        "upload": [
            { "hash": "sha256:1a2b", "parts": [ { "part_no": 1, "url": "https://s3.mock/put" } ] }
        ],
        "already_present": ["sha256:9f3c"]
    });
    (StatusCode::CREATED, Json(body))
}

async fn commit_dataset_version(Path((_id, vid)): Path<(String, String)>) -> Json<Value> {
    Json(json!({
        "version_id": vid,
        "ref": "ds:my-sft@v1",
        "manifest_root": "sha256:abcd",
        "immutable": true
    }))
}

async fn create_deployment() -> impl IntoResponse {
    let body = json!({
        "id": "dep_mock",
        "name": "my-model",
        "state": "warming",
        "endpoint": { "base_url": "https://inference.loom.dev/v1", "model": "my-model" },
        "replicas": { "desired": 1, "ready": 0 }
    });
    (StatusCode::CREATED, Json(body))
}

async fn get_deployment(Path(id): Path<String>) -> Json<Value> {
    Json(json!({
        "id": id,
        "name": "my-model",
        "state": "ready",
        "endpoint": { "base_url": "https://inference.loom.dev/v1", "model": "my-model" },
        "replicas": { "desired": 1, "ready": 1 },
        "live_qps": 3.5,
        "health": "healthy"
    }))
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;

    /// Substitute a concrete id into a templated route path.
    fn concretize(path: &str) -> String {
        path.replace("{id}", "abc123").replace("{vid}", "v1")
    }

    /// Send one request through the router in-process (no socket) and return the
    /// status plus the body bytes.
    async fn call(method: &str, path: &str) -> (StatusCode, Vec<u8>) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .expect("build request");
        let response = app().oneshot(request).await.expect("router responds");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("collect body");
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn every_golden_path_route_answers() {
        for (method, path) in GOLDEN_PATH_ROUTES {
            let uri = concretize(path);
            let (status, body) = call(method, &uri).await;
            assert!(
                status.is_success(),
                "{method} {uri} returned {status}, expected 2xx"
            );
            assert!(!body.is_empty(), "{method} {uri} returned an empty body");
        }
    }

    #[tokio::test]
    async fn create_job_returns_job_group_fixture() {
        let (status, body) = call("POST", "/v1/jobs").await;
        assert_eq!(status, StatusCode::CREATED);
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["kind"], "job_group");
        assert!(value["children"].as_array().is_some_and(|c| !c.is_empty()));
    }

    #[tokio::test]
    async fn get_job_echoes_the_path_id() {
        let (status, body) = call("GET", "/v1/jobs/job_xyz").await;
        assert_eq!(status, StatusCode::OK);
        let value: Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(value["id"], "job_xyz");
        assert!(value["state"].is_string());
    }

    #[tokio::test]
    async fn logs_route_is_an_event_stream() {
        let request = Request::builder()
            .method("GET")
            .uri("/v1/jobs/job_xyz/logs")
            .body(Body::empty())
            .expect("build request");
        let response = app().oneshot(request).await.expect("router responds");
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .expect("content-type header");
        assert!(content_type.starts_with("text/event-stream"));
    }
}
