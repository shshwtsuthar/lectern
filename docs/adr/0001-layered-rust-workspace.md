# ADR 0001: Use a layered Rust workspace

- Status: Accepted
- Date: 2026-08-24

## Context

Lectern needs room for a native interface, persistent storage, file-format support, and device
integrations. Binding those concerns directly to domain behavior would make testing and future
technology changes expensive.

## Decision

Use a Cargo workspace with `lectern-core` as the UI- and infrastructure-independent application
boundary. Executables and integrations depend inward on that crate. Start with a small CLI adapter
as a compilation and smoke-test surface; choose desktop and storage technologies separately when
their requirements are understood.

## Consequences

- Core behavior can be tested without a UI, filesystem, device, or database.
- Adapter technologies remain replaceable and platform-specific dependencies stay contained.
- Boundary types and interfaces require deliberate design.
- More crates are added only when dependency or ownership boundaries justify them.
