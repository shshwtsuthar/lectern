# Architecture decision records

Architecture decision records (ADRs) capture choices that are costly to reverse or affect multiple
parts of the system.

Copy the structure of an existing record, assign the next four-digit number, and use one of these
statuses: Proposed, Accepted, Superseded, or Rejected. Never rewrite an accepted decision to hide a
change; supersede it with a new record.

## Records

- [ADR 0001: Use a layered Rust workspace](0001-layered-rust-workspace.md)
- [ADR 0002: Model logical books separately from file assets](0002-model-logical-books-and-assets.md)
- [ADR 0003: Normalize curation metadata and retain hot book projections](0003-normalize-curation-metadata.md)
- [ADR 0004: Own a Primer-inspired GPUI UI layer](0004-own-a-primer-inspired-gpui-ui-layer.md)
