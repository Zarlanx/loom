// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! Host-agent configuration (`host-agent.md` §8: `/etc/loom/host.toml`).
//!
//! The agent loads a small TOML file at startup: where to reach the gateway, the
//! single-use enrollment token, and the durability knobs (heartbeat cadence, spool cap,
//! reconnect backoff). Everything here is owner-authoritative — a later control-plane
//! `ConfigPush` may *tighten* a value but never widen it (`agent-protocol.md` §3h); that
//! clamping lives with the policy engine, not this loader.

use std::{path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

/// Default heartbeat cadence (`agent-protocol.md` §3b: ~10–20 s while idle).
const DEFAULT_HEARTBEAT_MS: u64 = 15_000;
/// Default durable-spool cap before the agent stops accepting work (§3f).
const DEFAULT_SPOOL_CAP_BYTES: u64 = 64 * 1024 * 1024;
/// Reconnect backoff base (`agent-protocol.md` §1.3: base 500 ms).
const DEFAULT_BACKOFF_BASE_MS: u64 = 500;
/// Reconnect backoff cap (§1.3: ~30 s).
const DEFAULT_BACKOFF_CAP_MS: u64 = 30_000;

/// Errors from loading or validating [`HostdConfig`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("reading config {path}: {source}")]
    Read {
        /// The path that failed to read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The file was not valid TOML for [`HostdConfig`].
    #[error("parsing config {path}: {source}")]
    Parse {
        /// The path that failed to parse.
        path: PathBuf,
        /// The TOML deserialization error.
        source: toml::de::Error,
    },
    /// A field held a value the agent refuses to run with.
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// Reconnect backoff bounds (`agent-protocol.md` §1.3: exponential + full jitter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconnectConfig {
    /// Backoff base delay, milliseconds.
    #[serde(default = "default_backoff_base_ms")]
    pub base_ms: u64,
    /// Backoff cap, milliseconds.
    #[serde(default = "default_backoff_cap_ms")]
    pub cap_ms: u64,
}

const fn default_backoff_base_ms() -> u64 {
    DEFAULT_BACKOFF_BASE_MS
}
const fn default_backoff_cap_ms() -> u64 {
    DEFAULT_BACKOFF_CAP_MS
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            base_ms: DEFAULT_BACKOFF_BASE_MS,
            cap_ms: DEFAULT_BACKOFF_CAP_MS,
        }
    }
}

impl ReconnectConfig {
    /// The backoff base as a [`Duration`].
    #[must_use]
    pub const fn base(self) -> Duration {
        Duration::from_millis(self.base_ms)
    }

    /// The backoff cap as a [`Duration`].
    #[must_use]
    pub const fn cap(self) -> Duration {
        Duration::from_millis(self.cap_ms)
    }
}

/// The host agent's on-disk configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostdConfig {
    /// Gateway control-channel endpoint, e.g. `ws://loomd.local:8443/agent`
    /// (WSS in production; `agent-protocol.md` §1.1).
    pub control_endpoint: String,
    /// Single-use enrollment token minted when the owner links the machine
    /// (`host-agent.md` §4). Empty once the node holds a signed cert.
    #[serde(default)]
    pub enroll_token: String,
    /// Directory the agent owns for durable state (keystore + spool).
    pub data_dir: PathBuf,
    /// Advisory node name carried into the CSR subject nonce (the real `agent_id` is
    /// assigned by the control plane at enrollment; `agent-protocol.md` §1.2).
    #[serde(default)]
    pub agent_name: Option<String>,
    /// Heartbeat cadence, milliseconds.
    #[serde(default = "default_heartbeat_ms")]
    pub heartbeat_interval_ms: u64,
    /// Durable-spool cap in bytes.
    #[serde(default = "default_spool_cap_bytes")]
    pub spool_cap_bytes: u64,
    /// Reconnect backoff bounds.
    #[serde(default)]
    pub reconnect: ReconnectConfig,
}

const fn default_heartbeat_ms() -> u64 {
    DEFAULT_HEARTBEAT_MS
}
const fn default_spool_cap_bytes() -> u64 {
    DEFAULT_SPOOL_CAP_BYTES
}

impl HostdConfig {
    /// Loads and validates the config at `path`.
    ///
    /// # Errors
    ///
    /// - [`ConfigError::Read`] if the file cannot be read.
    /// - [`ConfigError::Parse`] if it is not valid TOML for this shape.
    /// - [`ConfigError::Invalid`] if a field fails [`validate`](Self::validate).
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();
        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        let config: Self =
            toml::from_str(&text).map_err(|source| ConfigError::Parse { path, source })?;
        config.validate()?;
        Ok(config)
    }

    /// Rejects values the agent must not run with.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Invalid`] when the endpoint is empty or the heartbeat
    /// interval is zero (a zero cadence would busy-loop the heartbeat task).
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.control_endpoint.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "control_endpoint must not be empty".to_string(),
            ));
        }
        if self.heartbeat_interval_ms == 0 {
            return Err(ConfigError::Invalid(
                "heartbeat_interval_ms must be greater than zero".to_string(),
            ));
        }
        if self.reconnect.base_ms == 0 || self.reconnect.cap_ms < self.reconnect.base_ms {
            return Err(ConfigError::Invalid(
                "reconnect.base_ms must be > 0 and <= reconnect.cap_ms".to_string(),
            ));
        }
        Ok(())
    }

    /// The heartbeat cadence as a [`Duration`].
    #[must_use]
    pub const fn heartbeat_interval(&self) -> Duration {
        Duration::from_millis(self.heartbeat_interval_ms)
    }

    /// The keystore path under [`data_dir`](Self::data_dir).
    #[must_use]
    pub fn keystore_path(&self) -> PathBuf {
        self.data_dir.join("keystore.json")
    }

    /// The durable-spool path under [`data_dir`](Self::data_dir).
    #[must_use]
    pub fn spool_path(&self) -> PathBuf {
        self.data_dir.join("spool.log")
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigError, HostdConfig};

    fn sample_toml() -> &'static str {
        r#"
control_endpoint = "ws://loomd.local:8443/agent"
enroll_token = "single-use-T"
data_dir = "/var/lib/loom-hostd"
agent_name = "rig-01"
heartbeat_interval_ms = 10000
"#
    }

    #[test]
    fn parses_and_applies_defaults() {
        let cfg: HostdConfig = toml::from_str(sample_toml()).expect("valid toml");
        cfg.validate().expect("valid config");
        assert_eq!(cfg.control_endpoint, "ws://loomd.local:8443/agent");
        assert_eq!(cfg.heartbeat_interval_ms, 10_000);
        // Unset knobs fall back to defaults.
        assert_eq!(cfg.spool_cap_bytes, 64 * 1024 * 1024);
        assert_eq!(cfg.reconnect.base_ms, 500);
        assert_eq!(cfg.reconnect.cap_ms, 30_000);
        assert_eq!(
            cfg.keystore_path().file_name().expect("file name"),
            "keystore.json"
        );
        assert_eq!(
            cfg.spool_path().file_name().expect("file name"),
            "spool.log"
        );
    }

    #[test]
    fn empty_endpoint_is_rejected() {
        let cfg = HostdConfig {
            control_endpoint: "   ".to_string(),
            enroll_token: String::new(),
            data_dir: "/tmp/x".into(),
            agent_name: None,
            heartbeat_interval_ms: 15_000,
            spool_cap_bytes: 1,
            reconnect: super::ReconnectConfig::default(),
        };
        assert!(matches!(cfg.validate(), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn zero_heartbeat_is_rejected() {
        let mut cfg: HostdConfig = toml::from_str(sample_toml()).expect("valid toml");
        cfg.heartbeat_interval_ms = 0;
        assert!(matches!(cfg.validate(), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let bad = "control_endpoint = \"ws://x\"\ndata_dir = \"/x\"\nbogus = 1\n";
        assert!(toml::from_str::<HostdConfig>(bad).is_err());
    }

    #[test]
    fn load_reads_from_disk() {
        let dir = std::env::temp_dir().join(format!("loom-hostd-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("host.toml");
        std::fs::write(&path, sample_toml()).expect("write");
        let cfg = HostdConfig::load(&path).expect("load");
        assert_eq!(cfg.agent_name.as_deref(), Some("rig-01"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_a_read_error() {
        let err = HostdConfig::load("/nonexistent/loom/host.toml").expect_err("should fail");
        assert!(matches!(err, ConfigError::Read { .. }));
    }
}
