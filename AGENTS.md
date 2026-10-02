# Project working rules

- Implement in tested, executable milestones. Current delivery includes Phase 6 inventory accounting; see docs/milestones.md and docs/phase6.md before extending it.
- Rust stable only in engine paths. No AI/ML/inference, floating-point execution money, databases, message brokers, or per-decision async scheduling.
- Use checked integer arithmetic, explicit units, bounded memory, and single ownership. Share production modules with replay.
- Native venue adapters must validate native sequence ranges and atomic depth batches before canonicalizing events. Do not treat canonical sequence IDs as native protocol validation.
- Document models, assumptions, state invariants, configuration, tests and benchmark conditions for each new component.
- A void revisit or partial penetration is sufficient for analysis. Never require a second full traversal.
- Portfolio recovery and bounded inventory are the research objectives. Never encode “never close a loser.” Hard risk controls override strategy.
- Placeholder interface crates are not working features. Keep their status explicit until implemented and tested.
- Run cargo fmt --all -- --check, cargo test --workspace --locked, and cargo clippy --workspace --all-targets --locked -- -D warnings for relevant changes.
- Book benchmarks: cargo bench -p book --bench book --locked and cargo bench -p book --bench profile --locked. Keep measurement code separate from execution code.
- Consolidation benchmarks: cargo bench -p consolidator --bench consolidation --locked and cargo bench -p consolidator --bench profile --locked.
- Binary v1 cannot encode atomic batch boundaries. Add a versioned batch envelope before recording native batch feeds; do not flatten batches and claim equivalent replay.
- Structure benchmarks: cargo bench -p liquidity --bench structures --locked and cargo bench -p liquidity --bench profile --locked. Keep coverage, sampling, corridor and source-mask assumptions explicit.
- Flow benchmarks: cargo bench -p orderflow --bench flow --locked and cargo bench -p orderflow --bench profile --locked. Preserve unknown cancellation attribution; Poisson event probability is not queue-fill probability. Time-window overflow must fail visibly.
- Inventory benchmarks: cargo bench -p inventory --bench ledger --locked and cargo bench -p inventory --bench profile --locked. Ledger commands are all-or-nothing; append-only history is committed only after success. A normal close must contribute net profit or improve the projected balance including committed closes; emergency exits after a halt must always remain possible. Acknowledged fills are never refused. Revisit evidence is a caller attestation until Phase 7 connects it.
