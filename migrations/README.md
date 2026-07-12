# migrations/

The **single** sqlx migration set — one logical history, dialect divergences documented
inline ([backend.md §4](../docs/platform/backend.md)). Lives at the repo root (not inside
`loom-store/`) so the SQL history is reviewable as its own artifact and `xtask migrate`
reads one canonical path. First migrations land with **PR-05** (`store-sqlite`).
