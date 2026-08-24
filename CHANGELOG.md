# Changelog

Notable changes to Lectern will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). The project will
follow [Semantic Versioning](https://semver.org/) once releases begin.

## [Unreleased]

### Added

- Confirmed single-book removal that clears library metadata, asset records, covers, and search
  entries without deleting the user's EPUB or PDF source files.
- A conservative performance-impact classifier and stable pull-request gate with a versioned suite
  registry, three-run paired base/candidate p95 comparisons, absolute budgets, and 90-day artifacts.
- Bounded 128-book library result pages with a six-page desktop cache, one queued query request,
  and deterministic 50,000-book first-page, deep-page, and filtered-page regression coverage.
- A deterministic 50,000-book release-query regression suite with versioned p95/relative budgets,
  local execution, weekly/manual GitHub Actions runs, and retained diagnostic artifacts.
- Referenced-asset health scans, missing/unreadable file reporting and filtering, and validated
  in-place relinking that preserves book metadata, covers, and stable asset IDs.
- Logical books with stable EPUB/PDF assets, explicit managed/reference ownership, reversible paths,
  atomic grouped imports, and direct migration from the earlier single-file schemas.
- Native desktop library browser with a virtualized cover grid and bounded texture cache.
- Indexed SQLite storage with transactional migrations, WAL mode, and FTS5 search.
- Parallel EPUB and PDF discovery and import with metadata and cover extraction.
- Search, format filtering, title/author/recent sorting, and background query coalescing.
- File, folder, and drag-and-drop import with progress and per-file failure reporting.
- In-place metadata editing with asynchronous saves and immediate search-index refresh.
- Initial Rust workspace, quality policy, CI, and project documentation.
