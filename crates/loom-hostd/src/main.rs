// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! `loom-hostd` binary — thin wiring over the [`loom_hostd`] library.
//!
//! Loads the owner's config, and — if the node is not yet enrolled — runs the token-only
//! CSR enrollment against the configured gateway and persists the granted identity to the
//! keystore. The steady-state agent loop (FSM, heartbeats, spool) is wired in PR-08b.

use std::path::PathBuf;

use anyhow::Context as _;
use loom_hostd::{
    BackoffPolicy, EnrolledNode, Enroller, EnrollmentRequest, HostdConfig, NodeProfile,
    PlaceholderCsr, SystemClock, WssConnector,
};

/// Default config path (`host-agent.md` §8).
const DEFAULT_CONFIG: &str = "/etc/loom/host.toml";
/// Bounded enrollment attempts before the binary gives up and exits non-zero.
const MAX_ENROLL_ATTEMPTS: u32 = 8;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config_path: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_CONFIG.to_string())
        .into();
    let config = HostdConfig::load(&config_path)
        .with_context(|| format!("loading config {}", config_path.display()))?;
    tracing::info!(endpoint = %config.control_endpoint, "loom-hostd starting");

    let keystore = config.keystore_path();
    if let Some(node) = EnrolledNode::load(&keystore).context("reading keystore")? {
        tracing::info!(agent_id = %node.agent_id, "already enrolled; steady-state loop lands in PR-08b");
        return Ok(());
    }

    let enroller = Enroller {
        csr: &PlaceholderCsr,
        clock: &SystemClock,
        request: EnrollmentRequest {
            token: config.enroll_token.clone(),
            subject_nonce: config
                .agent_name
                .clone()
                .unwrap_or_else(|| "loom-hostd".to_string()),
            profile: NodeProfile::default(),
        },
    };
    let connector = WssConnector::new(config.control_endpoint.clone());
    let backoff = BackoffPolicy::from(config.reconnect);
    // Full-jitter seed: not security-sensitive, just an anti-herd starting point.
    let seed = u64::from(std::process::id());

    let enrollment = enroller
        .enroll_with_backoff(&connector, backoff, seed, MAX_ENROLL_ATTEMPTS)
        .await
        .context("enrolling with the gateway")?;
    enrollment
        .node
        .save(&keystore)
        .context("persisting the node identity")?;
    tracing::info!(agent_id = %enrollment.node.agent_id, "enrolled; identity persisted");
    Ok(())
}
