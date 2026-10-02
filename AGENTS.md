# Project working rules

- Implement in tested, executable milestones. Current delivery includes Phase 8 live public feeds; see docs/milestones.md, docs/phase8.md and docs/providers.md before extending it.
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
- Recording v2 frames preserve atomic batch boundaries; never flatten a v2 batch (RecordingReader::next_event refuses v2). Native depth messages must reach the engine through apply_depth_batch.
- Structure benchmarks: cargo bench -p liquidity --bench structures --locked and cargo bench -p liquidity --bench profile --locked. Keep coverage, sampling, corridor and source-mask assumptions explicit.
- Flow benchmarks: cargo bench -p orderflow --bench flow --locked and cargo bench -p orderflow --bench profile --locked. Preserve unknown cancellation attribution; Poisson event probability is not queue-fill probability. Time-window overflow must fail visibly.
- Inventory benchmarks: cargo bench -p inventory --bench ledger --locked and cargo bench -p inventory --bench profile --locked. Ledger commands are all-or-nothing; append-only history is committed only after success. A normal close must contribute net profit or improve the projected balance including committed closes; emergency exits after a halt must always remain possible. Acknowledged fills are never refused. Revisit evidence is a caller attestation until Phase 7 connects it.
- Engine benchmarks: cargo bench -p engine --bench engine --locked and cargo bench -p engine --bench profile --locked. Engine::apply is the only entry point for direct input and replay. Fills are simulated by strict trade-through only; never present scenario PnL as edge. Entries require an allowed revisited zone AND balanced-active environment. Keep engine tests checking invariants after every event, and box engines in tests (debug stack).
- Live feeds: network code lives only in crates/feeds; venue crates are pure decoders with documented sync rules. Canonical receive time is the sequencer's stamp. Never forward trades or batches for a venue without a live book. Use price_reference = "composite" for live multi-venue data. Never commit recorded exchange data (terms); tests use synthetic messages in documented formats. Decode benchmarks: cargo bench -p feeds --bench decode --locked.
