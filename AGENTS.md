# Agent Instructions

## Rule #1: Make performance a core design priority

Lectern's primary goal is to be an exceptionally fast, highly performant Rust application.
Performance is a product requirement and a merge requirement, not a cleanup activity.

Treat a change as performance-sensitive unless it is clearly incapable of affecting runtime work.
Performance-sensitive changes include, without exception:

- Rust source changes in any runtime crate;
- dependency, feature-flag, toolchain, allocator, or release-profile changes;
- database schemas, migrations, indexes, transactions, and queries;
- filesystem discovery, parsing, image/PDF processing, imports, and persistence;
- rendering, layout, repaint behavior, texture handling, and other UI hot paths;
- worker scheduling, channels, locking, batching, pagination, caching, and memory ownership; and
- benchmark workloads, measurement code, budgets, or performance CI configuration.

Documentation, comments, tests that do not alter production code, and purely static copy changes may
be classified as non-performance-sensitive. A change being described as "UI-only" is not sufficient
for an exemption: rendering and interaction changes can affect frame time, allocation volume, and
memory use.

For every performance-sensitive change:

1. Identify the affected user journey and its representative workload before implementation.
2. Run the fastest relevant release-mode benchmark before every commit containing that change. The
   benchmark must retain raw samples, validate result correctness, and report p95 for latency or the
   corresponding tail/throughput and memory measures for that workload.
3. If no applicable benchmark exists, add a deterministic benchmark scenario and an explicit budget
   as part of the change. Do not commit an unmeasured hot path with a promise to benchmark it later.
4. Compare the candidate against its base revision on the same machine when a comparable baseline
   exists. A result must satisfy both the absolute product budget and the checked-in relative
   regression budget.
5. Use representative data sizes and production code paths. Debug builds, toy inputs, single timing
   samples, and unverified synthetic shortcuts are not performance evidence.
6. Prefer efficient algorithms and data structures; avoid unnecessary allocations, copying, I/O,
   wakeups, and unbounded work. Consider memory, concurrency, and cache behavior explicitly.
7. Preserve the raw benchmark artifacts and record the command and result in the commit or pull
   request validation notes.

Never weaken, remove, rename, or reduce a benchmark workload merely to make a regression pass. A
budget relaxation or workload-version change requires a separate, explicit performance-policy
change with measured evidence and user approval. If the relevant benchmark cannot run because of an
environment or tooling constraint, report the exact blocker immediately and do not commit the
performance-sensitive change.

The authoritative process, classifications, budgets, and commands are documented in
`docs/performance-policy.md`.

## Rule #2: Commit often and frequently

Make small, cohesive Git commits throughout every task. Commit after each independently useful, working change and before moving to a different concern. Do not wait until the end of a large task to create one oversized commit, and do not bundle unrelated changes.

Use concise Conventional Commit subjects:

```text
<type>(optional-scope): <imperative summary>
```

Use established types such as `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`, `ci`, and `chore`. Keep the subject specific, professional, and preferably no longer than 72 characters. Add a body only when the motivation, tradeoffs, or migration notes are not evident from the diff.

Before each commit, run the fastest relevant validation for the change. Each commit should be reviewable and leave the repository in a working state. Never rewrite, squash, or discard commits belonging to the user or another contributor unless the user explicitly requests it.

If Git is unavailable or a commit cannot be created, report the exact blocker immediately. Preserve the intended commit boundary so the change can be committed separately as soon as Git is usable.

## Rule #3: Follow Lectern's visual foundations

Every UI implementation and review must follow `docs/ui/visual-foundations.md`. Lectern's primary
brand color is Lectern Mauve (`#9B6AA6`), with Lectern Lavender (`#D8C4E1`) as its supporting light
tint. Introduce these through explicitly named Lectern theme tokens rather than component-local
literals or imported Primer names.

Lectern's design system is Primer-derived, not an exact Primer implementation. Lectern Mauve
replaces GitHub green in brand-primary roles, including primary actions. Preserve green for
independent semantic meanings such as success; do not use it as Lectern's default primary-action
color merely because Primer does.

Nested rounded surfaces that visually follow the same corner and have a uniform inset must use
concentric radii:

```text
innerRadius = max(outerRadius - inset, 0)
```

Do not reuse the outer radius blindly. Check the actual inset and the exceptions in the visual
foundations before implementing or approving nested corner geometry.
