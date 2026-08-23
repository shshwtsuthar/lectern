# Agent Instructions

## Rule #1: Make performance a core design priority

Lectern's primary goal is to be an exceptionally fast, highly performant Rust application. Treat performance as a first-class requirement when designing or changing core functionality: choose efficient algorithms and data structures, avoid unnecessary allocations and copying, and consider memory, concurrency, and cache behavior where they matter. Keep the resulting code correct, idiomatic, and maintainable, and validate performance-sensitive decisions with profiling or
benchmarks rather than assumptions.

## Rule #2: Commit often and frequently

Make small, cohesive Git commits throughout every task. Commit after each independently useful, working change and before moving to a different concern. Do not wait until the end of a large task to create one oversized commit, and do not bundle unrelated changes.

Use concise Conventional Commit subjects:

```text
<type>(optional-scope): <imperative summary>
```

Use established types such as `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`, `ci`, and `chore`. Keep the subject specific, professional, and preferably no longer than 72 characters. Add a body only when the motivation, tradeoffs, or migration notes are not evident from the diff.

Before each commit, run the fastest relevant validation for the change. Each commit should be reviewable and leave the repository in a working state. Never rewrite, squash, or discard commits belonging to the user or another contributor unless the user explicitly requests it.

If Git is unavailable or a commit cannot be created, report the exact blocker immediately. Preserve the intended commit boundary so the change can be committed separately as soon as Git is usable.
