# migrations/

The **single** sqlx migration set — one logical history, dialect divergences documented
inline ([backend.md §4](../docs/platform/backend.md)). Lives at the repo root (not inside
`loom-store/`) so the SQL history is reviewable as its own artifact and `xtask migrate`
reads one canonical path.

- `0001_init.sql` — the Phase-1 control-plane schema (control-plane.md §2): `accounts`,
  `api_keys`, `hosts`, `gpus`, `nodes`, `jobs`, `job_attempts`, `leases`, `usage_records`,
  `outbox`, `idempotency_keys`. Dialect conventions (IDs as TEXT ULIDs, epoch-ms
  timestamps, integer micro-USD money, TEXT JSON, 0/1 booleans) are noted inline for the
  future Postgres marketplace leg; Phase 1 is SQLite-only ([ADR-0013](../docs/adr/0013-single-binary-self-host-control-plane.md)).

`loom-store` embeds this set (`sqlx::migrate!`) and runs it on `SqliteStore::open`; the
same [`Migrator`] backs the CLI:

```
cargo xtask migrate --backend sqlite-wal --database loom.db          # apply pending
cargo xtask migrate --backend sqlite-wal --database loom.db --check  # verify; non-zero if pending
```

The store-conformance CI leg runs `xtask migrate` then the shared conformance suite
against a **file-backed WAL** database (never `:memory:`).
