// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Canonical golden wire vectors: one deterministic, fully-populated `Envelope` per M1
//! message shape, serialized to bytes.
//!
//! This module is the **single source of truth** for the checked-in vectors under
//! `crates/loom-proto/tests/golden/`. Two consumers read it and must never disagree:
//!
//! - `cargo xtask golden regen` writes each vector to `<name>.bin` — the blessed path for
//!   an intentional, reviewable schema change (workspace-setup.md §5).
//! - the `golden_vectors` integration test (CI job f) asserts each `<name>.bin` is
//!   byte-identical to what [`vectors`] produces, and that the bytes round-trip through
//!   decode/encode unchanged.
//!
//! Because both sides call [`vectors`], a stale checked-in file is the only way they can
//! diverge — which is exactly the drift the golden gate exists to catch.

use prost::Message;

use crate::v1::{
    AgentConfig, AgentState, AttemptState, Backend, BackendRuntime, BenchmarkFingerprint,
    EnrollGrant, EnrollRequest, Envelope, ExitClass, GpuTelemetry, HardwareInventory, Heartbeat,
    JobAbort, JobAccept, JobCompleted, JobFailed, JobManifest, JobOffer, JobPrepareProgress,
    JobReject, JobStarted, LogChunk, LogStream, MemoryModel, NatClass, NatProbeResult, ReEnroll,
    RejectReason, ResourceClaim, RotateCert, SignedManifest, StateReport, VersionRange,
    envelope::Body,
};

/// A single named golden vector: canonical serialized bytes of one `Envelope`.
#[derive(Debug, Clone)]
pub struct GoldenVector {
    /// Stable identifier; the checked-in file is `<name>.bin`.
    pub name: &'static str,
    /// Canonical serialized `Envelope` bytes.
    pub bytes: Vec<u8>,
}

/// Encodes `envelope` into a named golden vector.
fn vector(name: &'static str, envelope: &Envelope) -> GoldenVector {
    GoldenVector {
        name,
        bytes: envelope.encode_to_vec(),
    }
}

/// Fixed advisory timestamp shared by every vector (deterministic golden bytes).
const TS: i64 = 1_700_000_000_000;

/// A deterministic 26-character ULID-shaped id, left-padded with `0` for readability.
///
/// The value is advisory (the schema does not validate ULID structure); fixing it keeps
/// the golden bytes reproducible.
fn ulid(tag: &str) -> String {
    format!("{tag:0>26}")
}

/// The single attempt id shared across the job-lifecycle vectors, so they read as one
/// attempt's story.
fn attempt_id() -> String {
    ulid("ATTEMPT1")
}

/// A body-less `Envelope` with the given `msg_id` tag.
fn header(msg_tag: &str) -> Envelope {
    Envelope {
        protocol_version: 1,
        msg_id: ulid(msg_tag),
        correlation_id: String::new(),
        timestamp_ms: TS,
        body: None,
    }
}

/// An `Envelope` carrying `body`, tagged `msg_tag`, echoing `correlation` (empty if unsolicited).
fn envelope(msg_tag: &str, correlation: &str, body: Body) -> Envelope {
    Envelope {
        correlation_id: correlation.to_string(),
        body: Some(body),
        ..header(msg_tag)
    }
}

/// The canonical set of golden vectors for the frozen M1 message set.
///
/// Correlated responses (`enroll_grant`, `job_accept`, `job_reject`) echo the `msg_id` of
/// the request they answer, mirroring the request/response pairing on the control stream.
#[must_use]
pub fn vectors() -> Vec<GoldenVector> {
    let enroll_req = ulid("ENROLLREQ");
    let offer = ulid("JOBOFFER");
    vec![
        vector("envelope_header", &header("ENVHDR")),
        vector(
            "enroll_request",
            &envelope("ENROLLREQ", "", Body::EnrollRequest(enroll_request())),
        ),
        vector(
            "enroll_grant",
            &envelope(
                "ENROLLGRANT",
                &enroll_req,
                Body::EnrollGrant(enroll_grant()),
            ),
        ),
        vector(
            "re_enroll",
            &envelope("REENROLL", "", Body::ReEnroll(re_enroll())),
        ),
        vector(
            "rotate_cert",
            &envelope("ROTATECERT", "", Body::RotateCert(rotate_cert())),
        ),
        vector(
            "heartbeat",
            &envelope("HEARTBEAT", "", Body::Heartbeat(heartbeat())),
        ),
        vector(
            "state_report",
            &envelope("STATEREPORT", "", Body::StateReport(state_report())),
        ),
        vector(
            "job_offer",
            &envelope("JOBOFFER", "", Body::JobOffer(job_offer())),
        ),
        vector(
            "job_accept",
            &envelope("JOBACCEPT", &offer, Body::JobAccept(job_accept())),
        ),
        vector(
            "job_reject",
            &envelope("JOBREJECT", &offer, Body::JobReject(job_reject())),
        ),
        vector(
            "job_prepare_progress",
            &envelope(
                "JOBPREPARE",
                "",
                Body::JobPrepareProgress(job_prepare_progress()),
            ),
        ),
        vector(
            "job_started",
            &envelope("JOBSTARTED", "", Body::JobStarted(job_started())),
        ),
        vector(
            "job_completed",
            &envelope("JOBCOMPLETED", "", Body::JobCompleted(job_completed())),
        ),
        vector(
            "job_failed",
            &envelope("JOBFAILED", "", Body::JobFailed(job_failed())),
        ),
        vector(
            "job_abort",
            &envelope("JOBABORT", "", Body::JobAbort(job_abort())),
        ),
        vector(
            "log_chunk",
            &envelope("LOGCHUNK", "", Body::LogChunk(log_chunk())),
        ),
    ]
}

fn enroll_request() -> EnrollRequest {
    EnrollRequest {
        enroll_token: "enrol-token-single-use".to_string(),
        csr_der: b"pkcs10-csr-der".to_vec(),
        hw: Some(HardwareInventory {
            gpu_model: "Apple M3 Max".to_string(),
            vram_mb: 0,
            driver_version: "macOS 15.5".to_string(),
            cuda_version: String::new(),
            pcie_gen: 0,
            ecc: false,
            power_cap_w: 0,
            thermal_cap_c: 0,
            backends: vec![Backend::Mlx as i32, Backend::Cpu as i32],
            memory_model: MemoryModel::Unified as i32,
            unified_memory_mb: 49_152,
            backend_runtimes: vec![BackendRuntime {
                backend: Backend::Mlx as i32,
                version: "0.28.0".to_string(),
            }],
        }),
        bench: Some(BenchmarkFingerprint {
            tflops: 28.0,
            mem_bandwidth_gbps: 400.0,
            timing_p50_us: 120,
            timing_p99_us: 250,
        }),
        nat: Some(NatProbeResult {
            reflexive_addr: "203.0.113.7:41641".to_string(),
            nat_class: NatClass::Cone as i32,
        }),
        supported_versions: Some(VersionRange { min: 1, max: 1 }),
    }
}

fn enroll_grant() -> EnrollGrant {
    EnrollGrant {
        agent_id: "node-000000000000000000AGENT".to_string(),
        node_cert_der: b"node-cert-der".to_vec(),
        ca_chain_der: b"ca-chain-der".to_vec(),
        config: Some(AgentConfig {
            heartbeat_interval_ms: 15_000,
            spool_cap_bytes: 67_108_864,
            control_endpoint: "wss://loomd.local:8443/agent".to_string(),
            chosen_tiers: vec!["B".to_string()],
            config_version: "cfg-1".to_string(),
        }),
        chosen_version: 1,
    }
}

fn heartbeat() -> Heartbeat {
    Heartbeat {
        state: AgentState::Running as i32,
        gpu: Some(GpuTelemetry {
            util_pct: 87,
            vram_used_mb: 40_000,
            power_w: 55,
            temp_c: 61,
        }),
        cache_digest_version: "cache-v1".to_string(),
        monotonic_uptime_ms: 3_600_000,
        active_attempt_ids: vec![attempt_id()],
    }
}

fn state_report() -> StateReport {
    StateReport {
        state: AgentState::Running as i32,
        attempts: vec![AttemptState {
            attempt_id: attempt_id(),
            phase: "running".to_string(),
            last_seq_sent: 42,
            lease_fence: "7".to_string(),
        }],
        supported_versions: Some(VersionRange { min: 1, max: 1 }),
        cache_digest_version: "cache-v1".to_string(),
    }
}

fn job_offer() -> JobOffer {
    let manifest = JobManifest {
        image_digest: "sha256:0000000000000000000000000000000000000000000000000000000000000001"
            .to_string(),
        resource: Some(ResourceClaim {
            gpus: 1,
            min_vram_mb: 16_384,
            cpu: 4,
            ram_mb: 32_768,
        }),
        isolation_tier: "B".to_string(),
        trust_tier: "B".to_string(),
        max_duration_ms: 3_600_000,
        sealed_secrets_ref: "sealed-secrets-ref-01".to_string(),
        start_checkpoint_uri: "s3://loom-checkpoints/at1/ckpt-0".to_string(),
        backend: Backend::Mlx as i32,
    };
    JobOffer {
        attempt_id: attempt_id(),
        manifest: Some(SignedManifest {
            manifest_pb: manifest.encode_to_vec(),
            cp_sig: b"cp-signature".to_vec(),
            key_id: "cp-key-1".to_string(),
        }),
        lease_fence: "7".to_string(),
        lease_expires_ms: TS + 60_000,
    }
}

fn re_enroll() -> ReEnroll {
    ReEnroll {
        reason: "periodic re-verification".to_string(),
    }
}

fn rotate_cert() -> RotateCert {
    RotateCert {
        csr_der: b"pkcs10-csr-der".to_vec(),
    }
}

fn job_accept() -> JobAccept {
    JobAccept {
        attempt_id: attempt_id(),
    }
}

fn job_reject() -> JobReject {
    JobReject {
        attempt_id: attempt_id(),
        reason: RejectReason::Thermal as i32,
        detail: "gpu over thermal cap".to_string(),
    }
}

fn job_prepare_progress() -> JobPrepareProgress {
    JobPrepareProgress {
        attempt_id: attempt_id(),
        image_pct: 100,
        weights_pct: 60,
    }
}

fn job_started() -> JobStarted {
    JobStarted {
        attempt_id: attempt_id(),
        started_monotonic_ms: 3_600_500,
    }
}

fn job_completed() -> JobCompleted {
    JobCompleted {
        attempt_id: attempt_id(),
        exit_code: 0,
    }
}

fn job_failed() -> JobFailed {
    JobFailed {
        attempt_id: attempt_id(),
        exit_class: ExitClass::NodeFault as i32,
        detail: "owner_interrupt".to_string(),
    }
}

fn job_abort() -> JobAbort {
    JobAbort {
        attempt_id: attempt_id(),
        reason: "superseded by requeue".to_string(),
    }
}

fn log_chunk() -> LogChunk {
    LogChunk {
        attempt_id: attempt_id(),
        stream: LogStream::Stdout as i32,
        seq: 1,
        data: b"hello from echo\n".to_vec(),
        dropped_marker: false,
        dropped_count: 0,
    }
}
