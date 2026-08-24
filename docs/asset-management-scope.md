# Asset-management workflow scope

## Outcome

Lectern must complete the ordinary file-management workflows for its existing EPUB and PDF assets
before adding another publication parser. The work is an asset-management tranche over the current
logical-book model, not a format-expansion project.

The tranche is complete when a user can deliberately attach, open, locate, replace, detach, export,
or remove the records for the files they already manage. None of these actions may silently delete
a user-owned file.

## Current baseline

| Workflow | Current state | Work in this tranche |
| --- | --- | --- |
| Import, search, and edit metadata | Implemented | Preserve and regression-test |
| Scan and relink an unavailable referenced asset | Implemented | Keep relink limited to recovery |
| Remove a logical book from the library | Implemented and benchmarked | Verify copy and confirmation |
| Attach a missing EPUB or PDF format | Implemented and benchmarked | Verify copy and validation |
| Detach one asset | Missing | Implement |
| Open one asset | Missing | Implement |
| Reveal one asset in the file manager | Missing | Implement |
| Deliberately replace one asset | Missing | Implement separately from relink |
| Export one asset | Missing | Implement as an exact file copy |
| Delete a Lectern-managed copy | Blocked on managed storage | Do not expose yet |

No schema migration is expected. Stable asset IDs, formats, storage modes, health, and reversible
paths already provide the required persistence boundary.

## Product invariants

These rules are application contracts, not explanatory UI copy alone:

- **Remove book from library** deletes the logical-book database row, its asset rows, search data,
  and cached cover. It never deletes any referenced or managed publication file.
- **Detach asset** deletes exactly one asset relationship. It never deletes file bytes and the
  storage transaction must reject an operation that would leave the logical book with zero assets.
- **Attach another format** validates the selected publication through the existing bounded EPUB or
  PDF validation path, rejects a duplicate format or already-owned reference path, and adds the
  asset to the existing logical book. It does not change metadata, cover data, or existing assets.
- **Open book** launches the explicitly selected asset with the operating system's default
  application. Lectern does not parse, convert, or choose silently between multiple formats.
- **Reveal in file manager** opens the selected asset's parent folder and selects the file when the
  platform supports selection. Opening the parent folder is the documented fallback.
- **Relink asset** is recovery for a missing or unreadable referenced path. It validates a
  same-format replacement path while retaining the asset ID, metadata, and cover.
- **Replace asset** is a deliberate action available independently of file health. It validates a
  same-format replacement and retains the asset ID, logical book, metadata, and cover. Replacing a
  referenced asset never modifies or deletes the formerly referenced file.
- **Export asset** writes an exact byte-for-byte copy of the selected asset to a user-selected
  destination. It does not convert formats or mutate the library. An existing destination requires
  a separate overwrite confirmation, and a failed export must not leave a partial destination.
- **Delete managed copy** is not the consequence of remove, detach, or replace. It may be introduced
  only with managed storage, ownership/reference tracking, and its own explicit confirmation.

All mutating database operations are atomic. UI enablement is not an adequate invariant: storage
APIs must independently reject invalid last-asset detaches, wrong-format replacements, absent
books/assets, unavailable files, and reference-path conflicts.

## User experience

The metadata panel's **Files** section remains the home for asset-level actions. Each asset row must
show its format, ownership, health, and path, with **Open** as the primary action and a clearly named
**File actions** menu containing **Reveal in file manager**, **Export a copy**, **Replace file**, and
**Detach from book**. A file with an availability problem additionally exposes **Relink**. The
existing **Add EPUB/PDF** controls remain below the asset list, and **Remove from library** remains a
separate book-level action.

The selected asset is always named by format and path in destructive confirmations. Detach warns
that the file will remain on disk. Replace warns that the old referenced file will remain on disk.
Remove continues to state how many book files are unaffected. Failures remain in the open editor so
the user can retry without losing metadata edits.

Actions that only launch or copy a file do not require metadata changes to be saved. Mutating asset
actions are serialized with import, scan, attach, relink, metadata removal, and one another. Long
file copies run off the render thread and report progress; closing a dialog does not imply success.

## Implementation slices

### 1. Verify the completed book-level workflows

- Retain the current removal confirmation and its no-file-deletion storage test.
- Retain attachment validation, one-format-per-book enforcement, and source-byte assertions.
- Adjust labels only if necessary to use the product terms in this document.

This slice is verification/polish, not a rewrite of removal or attachment.

### 2. Detach an asset safely

- Add a transactional storage operation keyed by `AssetId` that resolves the owning book, counts
  its assets, rejects the last asset, and deletes only the selected row.
- Add an asset-maintenance worker request/event and refresh the selected book plus affected query
  page after success.
- Add the per-asset confirmation and precise error messages for a stale asset or last-asset attempt.
- Cover two-asset success, one-asset rejection, stale IDs, unchanged book metadata/cover, unchanged
  source bytes, and query/filter refreshes.

### 3. Open and reveal assets through a platform boundary

- Add a small desktop-owned platform adapter that accepts an exact `Path`; keep platform APIs and
  process launching out of `lectern-core` and `lectern-storage`.
- Implement default-application launch on Windows, macOS, and Linux.
- Implement file selection where supported and parent-folder fallback elsewhere.
- Check that the path is a readable regular file immediately before dispatch. Surface a recovery
  message instead of launching an unavailable path.
- Unit-test argument construction with a fake launcher, including spaces, quotes, and non-Unicode
  paths. Do not interpolate paths into a shell command.

Operating-system application startup is outside Lectern's latency boundary; dispatch and error
reporting must not block the render loop.

### 4. Separate replacement from relinking

- Keep **Relink** conditional on an unavailable referenced asset.
- Add a distinct **Replace file** action and confirmation for an intentionally selected asset.
- Share bounded publication validation and an internal transactional path-update helper, but expose
  separate relink and replace requests so their preconditions cannot be confused.
- Re-check asset identity, ownership, current availability, format, replacement availability, and
  path ownership at the storage boundary.
- Cover healthy replacement, missing-file relink, wrong-format rollback, path-conflict rollback,
  stable asset identity, unchanged metadata/cover, and preservation of both old and new source
  bytes.

For this tranche, relink and replace operate on referenced assets. Managed-asset replacement waits
for the managed-store ownership and resolution contract.

### 5. Export one selected asset

- Resolve the selected referenced asset and choose a destination with the native save dialog.
- Copy on a bounded worker using a fixed-size buffer and a temporary file in the destination
  directory, then atomically publish the completed copy where the platform permits.
- Never overwrite implicitly. Re-run destination checks after confirmation to handle races.
- Report progress, cancellation/failure, and final destination without changing asset health or any
  library row.
- Cover exact-byte success, destination collision, source disappearance, mid-copy failure, cleanup
  of temporary output, paths with non-ASCII/non-Unicode components, and a responsive UI during a
  representative large-file copy.

Managed-asset export is enabled only after the managed root can be resolved safely. Bulk export,
device export, conversion, and metadata sidecars are separate projects.

## Performance evidence

Every runtime slice is performance-sensitive under `docs/performance-policy.md` and must land with
release-mode evidence before its commit:

- Add a versioned 50,000-book detach lifecycle scenario. It must detach one of two assets, refresh
  the affected first page and format filter, verify the remaining logical book and source bytes,
  retain raw samples, and enforce absolute p95 plus paired relative budgets.
- Add a versioned replacement lifecycle scenario using the existing representative 8 MiB validated
  publication payload. It must include validation, the atomic update, first-page refresh, identity
  and byte-integrity checks, raw samples, and absolute p95 plus paired relative budgets.
- Exercise open/reveal through an injected no-op platform adapter in the compositor-backed desktop
  workload. Dispatch-to-next-painted-frame remains within the checked-in 50 ms p95 product budget;
  an external application's startup time is not measured as Lectern work.
- Add an export workload with a representative large file before implementing export. Gate UI
  dispatch-to-progress p95, copy throughput, peak memory, correctness, and temporary-file cleanup.
  The workload and explicit absolute/relative budgets must be checked in before the production copy
  path is committed.

Storage changes also run every registered deterministic storage suite so detach or replacement does
not regress import, query, attachment, removal, or refresh behavior. Raw benchmark artifacts and
commands are retained in the validation notes for each performance-sensitive commit.

## Commit boundaries

Implement as small working changes in this order:

1. Detach benchmark and storage contract.
2. Detach desktop workflow.
3. Platform adapter with open and reveal actions.
4. Replacement benchmark and storage contract.
5. Replacement desktop workflow.
6. Export benchmark and bounded copy engine.
7. Export desktop workflow and final copy review.

Each boundary must pass its focused tests and required release benchmark before commit. Do not
combine a benchmark-policy change or budget relaxation with the runtime feature it governs.

## Explicitly out of scope

- A third parser or any new publication format.
- Managed-library storage, migration into managed storage, managed-path resolution, garbage
  collection, or deletion of managed bytes.
- Deleting, moving, renaming, or modifying externally referenced files.
- Format conversion, DRM handling, password prompts, or book-content editing.
- Automatic replacement based on filename, title, metadata, or hash.
- Bulk detach, bulk replace, bulk export, device export, and Calibre-library import.
- Hashing, deduplication, and cross-book asset merging.

These exclusions prevent asset-management work from becoming an import, storage-ownership, or
format-expansion programme.
