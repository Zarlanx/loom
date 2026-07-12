-- Copyright 2026 Loom Contributors
-- SPDX-License-Identifier: Apache-2.0
--
-- Loom control-plane schema — one logical history (control-plane.md §2,
-- backend.md §4). Phase 1 is SQLite-only (ADR-0013); the dialect divergences we
-- would carry for the Postgres marketplace leg are noted inline so this file
-- stays the single source of truth when `PgStore` lands at scale.
--
-- Dialect conventions applied throughout (backend.md §4):
--   * IDs are app-generated ULIDs stored as TEXT. Postgres: UUID DEFAULT
--     gen_random_uuid(). We mint IDs in loom-core so both dialects receive an
--     explicit id (and tests stay deterministic).
--   * Timestamps are epoch-milliseconds INTEGERs. Postgres: TIMESTAMPTZ.
--   * Money is integer micro-USD (BIGINT-range INTEGER). Never NUMERIC/decimal.
--   * JSON payloads are TEXT. Postgres: JSONB.
--   * Booleans are INTEGER 0/1. Postgres: BOOLEAN.
--
-- Referential integrity: the enrollment/auth cluster (accounts, hosts, gpus,
-- nodes, api_keys) carries enforced foreign keys — auth safety depends on it,
-- and PRAGMA foreign_keys=ON is set per connection. The core execution tables
-- (jobs, job_attempts, leases, usage_records, outbox, idempotency_keys) are
-- written only by the single-writer scheduler in dependency order and their
-- cross-table invariants (the fencing lineage) are enforced in loom-core, so
-- they are kept FK-free here — this lets each surface be exercised in isolation
-- by the store conformance suite exactly as the in-memory FakeStore is.

-- ------------------------------------------------------------ accounts / auth

CREATE TABLE accounts (
    id         TEXT    PRIMARY KEY,
    name       TEXT    NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE api_keys (
    id         TEXT    PRIMARY KEY,
    account_id TEXT    NOT NULL REFERENCES accounts (id),
    key_hash   TEXT    NOT NULL UNIQUE,   -- hash of the presented token; never the token
    label      TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    revoked    INTEGER NOT NULL DEFAULT 0 -- 0/1; a revoked key never authenticates
);
CREATE INDEX api_keys_by_hash ON api_keys (key_hash);

-- --------------------------------------------------------------- enrollment

CREATE TABLE hosts (
    id           TEXT    PRIMARY KEY,
    account_id   TEXT    NOT NULL REFERENCES accounts (id),
    agent_pubkey BLOB    NOT NULL,       -- Postgres: BYTEA
    status       TEXT    NOT NULL,       -- 'pending' | 'enrolled' | 'revoked'
    enrolled_at  INTEGER NOT NULL,
    last_seen_at INTEGER
);

CREATE TABLE gpus (
    id          TEXT    PRIMARY KEY,
    host_id     TEXT    NOT NULL REFERENCES hosts (id),
    model       TEXT    NOT NULL,
    memory_mb   INTEGER NOT NULL,
    fingerprint TEXT                     -- benchmark fingerprint; NULL until taken
);
CREATE INDEX gpus_by_host ON gpus (host_id);

-- A concrete, schedulable capacity offer (control-plane §2). Mirrors loom-core
-- `Node`: a host + advertised backends + memory model + isolation + price.
CREATE TABLE nodes (
    id                      TEXT    PRIMARY KEY,
    host_id                 TEXT    NOT NULL REFERENCES hosts (id),
    status                  TEXT    NOT NULL, -- offline|available|leased|draining|owner_ejected
    gpu_model               TEXT    NOT NULL,
    memory_kind             TEXT    NOT NULL, -- 'unified' | 'discrete'
    memory_mb               INTEGER NOT NULL,
    backends                TEXT    NOT NULL, -- CSV of backend names, e.g. 'mlx,cpu'
    driver_version          TEXT    NOT NULL, -- 'major.minor.patch'
    cuda_version            TEXT,             -- NULL on non-CUDA nodes
    isolation_tier          TEXT    NOT NULL, -- 'B' | 'A' | 'C'
    region                  TEXT    NOT NULL,
    reliability_milli       INTEGER NOT NULL, -- thousandths, 0..=1000
    price_per_sec_micro_usd INTEGER NOT NULL, -- integer micro-USD
    last_heartbeat_at       INTEGER
);
CREATE INDEX nodes_by_status ON nodes (status, region, isolation_tier, gpu_model);

-- ------------------------------------------------------------------- jobs

CREATE TABLE jobs (
    id             TEXT    PRIMARY KEY,
    account_id     TEXT    NOT NULL,
    image_ref      TEXT    NOT NULL,
    workload_class TEXT    NOT NULL,        -- 'batch' | 'serving'
    priority       INTEGER NOT NULL DEFAULT 0,
    checkpoint_uri TEXT,
    state          TEXT    NOT NULL,        -- control-plane §3 lifecycle
    submitted_at   INTEGER NOT NULL,
    terminal_at    INTEGER,
    -- resource_claim: control-plane §2 stores this as one JSONB column; SQLite
    -- has no JSONB and loom-core's `ResourceClaim` is not serde-serializable, so
    -- its fields are flattened into typed, individually-filterable columns.
    claim_min_memory_mb         INTEGER NOT NULL,
    claim_gpu_model             TEXT,
    claim_min_driver            TEXT,       -- 'major.minor.patch' | NULL
    claim_min_cuda              TEXT,
    claim_min_isolation         TEXT    NOT NULL,
    claim_min_reliability_milli INTEGER NOT NULL,
    claim_region_pref           TEXT,
    claim_max_price_micro_usd   INTEGER NOT NULL,
    claim_backend_selector      TEXT    NOT NULL, -- 'auto' | 'only:<backend>'
    claim_supported_backends    TEXT    NOT NULL  -- CSV of backend names
);
CREATE INDEX jobs_by_state ON jobs (state, workload_class, priority DESC, submitted_at);
CREATE INDEX jobs_by_account ON jobs (account_id);

-- One placement of a job on a node; a job has many across a requeue lineage.
CREATE TABLE job_attempts (
    id                   TEXT    PRIMARY KEY,
    job_id               TEXT    NOT NULL,
    attempt_no           INTEGER NOT NULL,   -- 1,2,3… monotone per job
    node_id              TEXT,
    lease_id             TEXT,
    fence                INTEGER,            -- FenceToken value; NULL until leased
    phase                TEXT    NOT NULL,   -- control-plane §2 attempt phase
    start_checkpoint_uri TEXT,
    end_checkpoint_uri   TEXT,
    last_event_at        INTEGER,            -- drives the silence timeout
    UNIQUE (job_id, attempt_no)
);
CREATE INDEX job_attempts_by_job ON job_attempts (job_id);

-- The scheduler's exclusive, expiring claim on a node, carrying the fence.
CREATE TABLE leases (
    id         TEXT    PRIMARY KEY,
    attempt_id TEXT    NOT NULL,
    node_id    TEXT    NOT NULL,
    fence      INTEGER NOT NULL,   -- the split-brain guard's version number
    granted_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    state      TEXT    NOT NULL    -- 'active' | 'expired' | 'released'
);
-- The fencing compare-and-set reads the live claim on a node by this index.
CREATE INDEX leases_active_by_node ON leases (node_id, state, fence);

-- Signed per-window meter readings; idempotent on (attempt_id, seq).
CREATE TABLE usage_records (
    attempt_id    TEXT    NOT NULL,
    node_id       TEXT    NOT NULL,
    host_id       TEXT    NOT NULL,
    fence         INTEGER NOT NULL,   -- a stale fence is quarantined before billing
    seq           INTEGER NOT NULL,   -- monotone per attempt; a gap is a fraud signal
    window_start  INTEGER NOT NULL,
    window_end    INTEGER NOT NULL,
    billable_secs INTEGER NOT NULL,
    gpu_util_pct  INTEGER,
    validation    TEXT    NOT NULL,   -- 'pending' | 'valid' | 'implausible' | 'disputed'
    PRIMARY KEY (attempt_id, seq)     -- idempotent ingest (control-plane §2)
);

-- The transactional outbox: a state-change *nudge* written with the change it
-- announces (control-plane §3). At-least-once; consumers reconcile from state.
CREATE TABLE outbox (
    id         INTEGER PRIMARY KEY AUTOINCREMENT, -- monotone, never reused
    topic      TEXT    NOT NULL,
    payload    TEXT    NOT NULL,                  -- JSON text; Postgres: JSONB
    created_at INTEGER NOT NULL,
    sent_at    INTEGER                            -- NULL until the relay publishes it
);
CREATE INDEX outbox_unsent ON outbox (id) WHERE sent_at IS NULL;

-- The renter-facing idempotency window (renter-api §1.3): (account, key) → the
-- original response, so a retried POST replays rather than re-executes.
CREATE TABLE idempotency_keys (
    account_id      TEXT    NOT NULL,
    idempotency_key TEXT    NOT NULL,
    request_hash    TEXT    NOT NULL,   -- a reuse with a different body is a conflict
    response_status INTEGER NOT NULL,
    response_body   TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    PRIMARY KEY (account_id, idempotency_key)
);
