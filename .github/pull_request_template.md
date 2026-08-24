## Summary

<!-- What changed, and why? -->

## Validation

<!-- List automated and manual checks performed. -->

- [ ] Tests added or updated where behavior changed
- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy-all`
- [ ] `cargo test-all`
- [ ] Documentation and changelog updated where needed

## Performance impact

<!-- Select exactly one. Runtime Rust and dependency/profile changes cannot be classified as None. -->

- [ ] None — cannot affect runtime work
- [ ] Potential — may affect a measured path
- [ ] Material — changes or adds a hot path

Affected user journey and workload:

<!-- Required for Potential or Material. -->

Benchmark commands and before/after p95, throughput, frame, or memory results:

<!-- Required for Potential or Material. Link retained raw artifacts when available. -->

- [ ] Applicable deterministic scenario and budget added or updated
- [ ] Candidate passes applicable absolute and relative regression budgets
- [ ] No benchmark workload or budget was weakened to obtain a pass

## Risk and rollout

<!-- Note data migration, compatibility, performance, security, or rollback concerns. -->
