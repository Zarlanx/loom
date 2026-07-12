// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! In-crate fake gateway + duplex connector (`workspace-setup.md` §6: the fake lives
//! beside the code it exercises, behind a feature).
//!
//! [`FakeGateway`] speaks the **real** frozen wire protocol over a real WebSocket
//! handshake, but the socket underneath is an in-process [`tokio::io::duplex`] pipe — so
//! the agent enrolls, heartbeats, and reconnects with **no real network and no GPU**. The
//! same conformance the real gateway (`loom-agentproto`, PR-09) must meet: it decodes the
//! agent's `Envelope`s, grants a (fake-signed) cert on a valid token, and refuses a bad
//! one with a terminal close reason.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};

use loom_proto::{
    Body, Envelope,
    codec::Channel,
    v1::{AgentConfig, EnrollGrant, EnrollRequest, Heartbeat, StateReport},
};

use crate::{
    clock::{FixedClock, WallClock},
    connect::Connector,
    transport::{TransportError, WsTransport},
    wire::{MsgIdGen, envelope},
};

/// Everything the fake gateway captured, for test assertions.
#[derive(Debug, Default)]
pub struct GatewayRecord {
    /// Every `EnrollRequest` received (one per connection that reached bootstrap).
    pub enroll_requests: Vec<EnrollRequest>,
    /// Every `Heartbeat` received on the heartbeat channel.
    pub heartbeats: Vec<Heartbeat>,
    /// Every `StateReport` received (reconnect resyncs).
    pub state_reports: Vec<StateReport>,
    /// Every other control-channel envelope received (accepts, terminal reports, replays).
    pub control: Vec<Envelope>,
    /// Count of connections that completed a successful enrollment grant.
    pub grants: u32,
}

/// Static behaviour of the fake gateway.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// Tokens that receive a grant; anything else is refused with a terminal close.
    pub valid_tokens: Vec<String>,
    /// The `agent_id` assigned in the grant.
    pub agent_id: String,
    /// Config returned in the grant.
    pub grant_config: Option<AgentConfig>,
    /// Drop the connection immediately after granting (exercises the reconnect path).
    pub close_after_enroll: bool,
    /// Control-channel envelopes to push to the agent right after the grant (e.g. a
    /// scripted `JobOffer`).
    pub scripted: Vec<Envelope>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            valid_tokens: vec!["single-use-T".to_string()],
            agent_id: "node-fake-0000000000000001".to_string(),
            grant_config: Some(AgentConfig {
                heartbeat_interval_ms: 15_000,
                spool_cap_bytes: 64 * 1024 * 1024,
                control_endpoint: "ws://loom.test/agent".to_string(),
                chosen_tiers: vec!["B".to_string()],
                config_version: "cfg-fake-1".to_string(),
            }),
            close_after_enroll: false,
            scripted: Vec::new(),
        }
    }
}

/// A fake agent-gateway that serves one connection at a time over a supplied stream.
#[derive(Debug, Clone)]
pub struct FakeGateway {
    config: Arc<GatewayConfig>,
    record: Arc<Mutex<GatewayRecord>>,
}

impl Default for FakeGateway {
    fn default() -> Self {
        Self::new(GatewayConfig::default())
    }
}

impl FakeGateway {
    /// A gateway with the given behaviour.
    #[must_use]
    pub fn new(config: GatewayConfig) -> Self {
        Self {
            config: Arc::new(config),
            record: Arc::new(Mutex::new(GatewayRecord::default())),
        }
    }

    /// Snapshot of the enroll requests seen so far.
    #[must_use]
    pub fn enroll_requests(&self) -> Vec<EnrollRequest> {
        self.with_record(|r| r.enroll_requests.clone())
    }

    /// Snapshot of the heartbeats seen so far.
    #[must_use]
    pub fn heartbeats(&self) -> Vec<Heartbeat> {
        self.with_record(|r| r.heartbeats.clone())
    }

    /// Snapshot of the state reports seen so far.
    #[must_use]
    pub fn state_reports(&self) -> Vec<StateReport> {
        self.with_record(|r| r.state_reports.clone())
    }

    /// Snapshot of the other control-channel envelopes seen so far.
    #[must_use]
    pub fn control(&self) -> Vec<Envelope> {
        self.with_record(|r| r.control.clone())
    }

    /// Number of connections that completed a successful grant.
    #[must_use]
    pub fn grant_count(&self) -> u32 {
        self.with_record(|r| r.grants)
    }

    fn with_record<T>(&self, f: impl FnOnce(&GatewayRecord) -> T) -> T {
        // Poisoning only happens if a serve task panicked; recover the guard so assertions
        // still read what was captured.
        let guard = self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&guard)
    }

    fn record_mut(&self, f: impl FnOnce(&mut GatewayRecord)) {
        let mut guard = self
            .record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut guard);
    }

    /// Runs one connection to completion over `stream`: WebSocket accept, enrollment, then
    /// steady-state recording until the agent disconnects.
    pub async fn serve<S>(self, stream: S)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let mut transport = WsTransport::new(ws);
        let ids = MsgIdGen::new();
        let clock = FixedClock(0);

        // Bootstrap: the token-only connection may send only an EnrollRequest.
        let Ok((_channel, env)) = transport.recv().await else {
            return;
        };
        let correlation = env.msg_id.clone();
        let Some(Body::EnrollRequest(req)) = env.body else {
            let _ = transport.close_with_reason("expected_enroll_request").await;
            return;
        };
        let token_ok = self.config.valid_tokens.contains(&req.enroll_token);
        self.record_mut(|r| r.enroll_requests.push(req));
        if !token_ok {
            let _ = transport.close_with_reason("enroll_token_invalid").await;
            return;
        }

        let grant = EnrollGrant {
            agent_id: self.config.agent_id.clone(),
            node_cert_der: fake_sign(b"node"),
            ca_chain_der: b"LOOM-FAKE-CA".to_vec(),
            config: self.config.grant_config.clone(),
            chosen_version: 1,
        };
        let reply = envelope(
            ids.next_id(),
            correlation,
            clock.now_unix_ms(),
            Body::EnrollGrant(grant),
        );
        if transport.send(Channel::Control, &reply).await.is_err() {
            return;
        }
        self.record_mut(|r| r.grants += 1);

        if self.config.close_after_enroll {
            let _ = transport.close().await;
            return;
        }

        for scripted in &self.config.scripted {
            if transport.send(Channel::Control, scripted).await.is_err() {
                return;
            }
        }

        self.steady_state(&mut transport).await;
    }

    /// Records everything the agent sends after enrollment until it disconnects.
    async fn steady_state<S>(&self, transport: &mut WsTransport<S>)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        loop {
            match transport.recv().await {
                Ok((_channel, env)) => match env.body {
                    Some(Body::Heartbeat(hb)) => self.record_mut(|r| r.heartbeats.push(hb)),
                    Some(Body::StateReport(sr)) => self.record_mut(|r| r.state_reports.push(sr)),
                    _ => self.record_mut(|r| r.control.push(env)),
                },
                Err(_) => return,
            }
        }
    }
}

/// A fake "signature": the fixed prefix tags the bytes as non-cryptographic in any capture.
fn fake_sign(subject: &[u8]) -> Vec<u8> {
    let mut out = b"LOOM-FAKE-CERT:".to_vec();
    out.extend_from_slice(subject);
    out
}

/// A [`Connector`] that wires the agent to a [`FakeGateway`] over an in-process duplex,
/// optionally failing the first `fail_first` connect attempts to exercise reconnect
/// backoff.
#[derive(Debug, Clone)]
pub struct DuplexConnector {
    gateway: FakeGateway,
    fail_first: u32,
    attempts: Arc<AtomicU32>,
    buffer: usize,
}

impl DuplexConnector {
    /// A connector serving `gateway`, never failing a connect.
    #[must_use]
    pub fn new(gateway: FakeGateway) -> Self {
        Self {
            gateway,
            fail_first: 0,
            attempts: Arc::new(AtomicU32::new(0)),
            buffer: 64 * 1024,
        }
    }

    /// Fails the first `n` connect attempts with a transient error before succeeding.
    #[must_use]
    pub fn failing_first(mut self, n: u32) -> Self {
        self.fail_first = n;
        self
    }

    /// Total connect attempts observed (including the failed ones).
    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.attempts.load(Ordering::SeqCst)
    }
}

impl Connector for DuplexConnector {
    type Stream = tokio::io::DuplexStream;

    async fn connect(&self) -> Result<WsTransport<Self::Stream>, TransportError> {
        let n = self.attempts.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_first {
            return Err(TransportError::Connect(format!(
                "injected connect failure #{}",
                n + 1
            )));
        }
        let (client_io, server_io) = tokio::io::duplex(self.buffer);
        let gateway = self.gateway.clone();
        tokio::spawn(async move { gateway.serve(server_io).await });
        let (ws, _resp) = tokio_tungstenite::client_async("ws://loom.test/agent", client_io)
            .await
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        Ok(WsTransport::new(ws))
    }
}
