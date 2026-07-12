# proto/

Authoritative `.proto` sources for the wire protocol (`Envelope` + message catalog).
`prost-build` reads these; generated Rust lives in `loom-proto`. Lands with **PR-02**
(`proto-contract`) and evolves **additively only** thereafter
([agent-protocol.md §2.3](../docs/platform/agent-protocol.md)).
