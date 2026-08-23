# Architecture

Lectern uses a Cargo workspace so deployable applications can remain thin adapters around a stable,
testable core.

## Dependency direction

```text
executable / UI adapters  ──>  lectern-core  <──  infrastructure adapters
```

`lectern-core` owns domain language, use cases, and the interfaces those use cases need. It must not
depend on a desktop framework, database driver, device SDK, network client, or platform API. Adapters
may depend on the core and implement its interfaces; the core must never depend on adapters.

The CLI exists to prove the executable boundary and give automation a lightweight smoke-test target.
It is not a commitment to a CLI-first product.

## Planned boundaries

Add crates only when a boundary has distinct dependencies, ownership, or release needs. Likely
future boundaries include a desktop application, durable storage, format parsing, and device/export
adapters. Avoid a crate per feature and avoid a shared `utils` crate; keep helpers with the concepts
that own them.

## Operational principles

- Treat the user's ebook files and metadata as durable data: migrations must be explicit,
  transactional where possible, and recoverable.
- Keep indexing and import work cancellable and observable; large-library performance needs
  repeatable benchmarks before claims are made.
- Use structured diagnostics internally and translate them to actionable messages at application
  boundaries.
- Keep platform-specific code behind adapters and exercise the workspace on all supported operating
  systems in CI.

Architecture decisions that constrain future work are recorded in `docs/adr/`.
