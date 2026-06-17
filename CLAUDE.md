# CLAUDE.md — dcs Project

DO NOT ever run git commands for stage, commits, branches, merges, rebases, or anything that modifies history.

## Project

dcs (Digital Contact Sheet) is a fast, keyboard-first photo contact sheet in Rust — scan, cull, tag, export. Cargo workspace, 4 crates under `crates/`. The authoritative design lives in `spec.md`.

## Golden Rule

**Read `spec.md` before making architectural decisions.** If your plan conflicts with the spec, STOP and ask the user. Never silently deviate. If you think the spec is wrong, say so and explain why — don't silently override it.

## Layer Architecture — ENFORCED

Arrows point down only. Enforced at compile time.

```
dcs-ui      egui binary: grid + gallery view modes, ephemeral UI state  → dcs-app, dcs-domain (types)
dcs-app     conductor: session, command registry, dispatch, keymap, durable undo → dcs-io, dcs-domain
dcs-io      infrastructure behind traits: imaging / source / persistence → dcs-domain
dcs-domain  pure core: types + pure functions (no I/O, no async, no egui) → no internal deps
```

**Violations that must never happen:**
- UI calling `dcs-io` directly. UI talks to `dcs-app` via the command registry; it may use `dcs-domain` types.
- Any layer depending on a layer above it. Communication upward is events/channels only.
- Leaking `egui` types below `dcs-ui`.
- Leaking infrastructure types (`rusqlite`, `image`, exif) above `dcs-io`. Hide them behind traits.
- I/O concepts (paths-as-errors, `io::Error`) leaking into `dcs-domain`. Domain owns its own error enums; domain failures never come up from `dcs-io`.

When adding a `use` import, verify it respects this graph.

## Derived vs Owned — load-bearing rule (spec §2.2)

- **Owned (persisted):** verdicts, tags + assignments, views. Goes in `project.json`.
- **Derived (never persisted):** grouping, bursts, sort, titles, counts. Recomputed from metadata + settings.
- **Never persist anything derived.** If it can be recomputed, it does not go in the project file.
- Per-photo facts (verdict, tags) live on the photo, true in every view. Layout facts (position, membership) live on the view.

## Rust Standards

- **Public items first.** In every file, all `pub` types, functions, and impls go above private items.
- `thiserror` for all error enums. Per-module error types, never one global error type. Each module owns its error enum, re-exported from the module root.
- No `.unwrap()` or `.expect()` in library crates except for proven invariants with a comment explaining why. Panics are bugs, not error handling.
- `Result<T, E>` for anything that can fail. Fallible constructors return `Result`; reserve `new()` for infallible creation.
- One responsibility per function. Orchestration functions that call a sequence of single-purpose functions are fine.
- Prefer explicit types at API boundaries. Infer internally.
- Prefer `&[T]`/`&str` in signatures that only read. Store `String`, accept `&str` or `impl Into<String>`.
- `Command` (the registry enum) is serialized to `undo.log`. Treat it as append-only — never remove or rename variants; this breaks replay of existing logs.
- Don't use `mod.rs`; use named files (`foo.rs`, not `foo/mod.rs`).
- **Module exports.** `lib.rs` declares `pub mod` for top-level modules. Each module root re-exports public types from its private subfiles via `pub use`. Consumers import from one level: `dcs_domain::export::ExportPlan`, never `dcs_domain::export::types::ExportPlan`.
- **Keep big modules split by concern.** `session` (`crates/dcs-app/src/session/`) and `app` (`crates/dcs-ui/src/app/`) are directory modules: the parent file holds the struct + lifecycle, cohesive concerns live in child files (`session/display.rs`, `app/menus.rs`, …) as `impl` blocks. When adding to `Session`/`DcsApp`, put new code in the child that fits its concern; spin up a new child file when a *distinct* concern emerges — a trait + its impls, a self-contained subsystem, a cluster of related methods. Only split when it earns its keep: don't fragment one cohesive concern across files or add a file for two methods. Don't let the parent file grow back into a dumping ground.
- **Comments discipline:** `///` on all public functions/types. Private functions get doc comments only when the logic isn't obvious. No section-separator/banner comments. Inline comments only where the *why* isn't clear from the code.
- **No spec-reference noise.** Never write comments whose only content is a spec pointer (`§2.13`, `#34`, `(spec §6.9)`, `open Q#8`) — they add tokens and visual noise without explaining anything. Don't append `(§X)` tags to otherwise-fine comments either. A comment must explain a *why* or a non-obvious *what* that the code itself doesn't; if it only points at the spec or restates the code, delete it. Trace design decisions through `spec.md` and commit messages, not inline tags.

## Error Handling

- Errors carry context: `ExportError::BadTemplate(String)`, not a bare `io::Error`.
- Use `#[from]` in `thiserror` sparingly — only when the conversion is unambiguous. Prefer explicit `.map_err()` to add context.
- Never discard errors with `let _ = ...` unless documented why.

## Threading (spec §10) — no tokio in v1

egui main thread never blocks: I/O requests return handles instantly. rayon decode pool (≈ physical cores) for JPEG/preview decode; one-directional channels for results. All threading lives inside `dcs-io` behind handle-returning traits.

- No `.block_on()` of any kind — there is no async runtime in v1.
- Never hold a `Mutex`/`RwLock` guard across a channel recv or a long operation. Prefer message passing over shared locks.
- No locks on the UI thread's hot path. Send a request, receive an event.
- rayon closures must be `Send`. Don't capture `Rc`, non-Send guards, or UI state.
- Cancellation (export, decode) is checked between work units, never mid-operation.
- Channel sends from the UI thread are non-blocking. A full channel must never freeze the event loop.

## Storage & Safety (spec §10b)

- **Originals are sacred. dcs never deletes files.** Rejecting is metadata; export-rejected + reveal-rejected are the only exits.
- **Atomic writes:** `project.json` and every export copy use `.part`/`.tmp` → fsync → rename. A crash leaves old or new, never torn. Keep a rotated `.bak`.
- Three stores, distinct contracts: `project.json` precious, `undo.log` durable-but-rebuildable, `cache.sqlite3` disposable. Corruption of a disposable/durable store must never cost owned user state.
- File identity is the **content fingerprint**, not the path (mtime+size as a fast pre-filter). Renamed-on-disk keeps its verdicts and tags.

## Export — pure planner, dumb executor (spec §6.9)

- All export logic (scope, file selection, layout, collision renaming, name templates) is **pure and lives in `dcs-domain::plan_export` → `ExportPlan`**. No disk access there.
- `dcs-io` only executes the plan: walks `plan.ops`, copies files, emits progress, supports cancel. It makes **no** path/rename/skip decisions.
- The dialog's live preview *is* the `ExportPlan`. Never compute the summary and the run on two code paths.
- Copy-only in v1. **Never overwrite** — skip or rename `-1, -2…`.

## Unsafe Discipline

- All `unsafe` blocks get a `// SAFETY:` comment explaining the invariant. No exceptions.
- v1 has essentially no FFI; avoid `unsafe` entirely unless there's a documented, user-approved reason.

## Testing — Non-Negotiable

**Every task includes tests.**

- `dcs-domain` is pure — unit-test it exhaustively, zero mocks. The export planner especially: collision cascades, RAW-only-with-missing-RAW, template × split-layout, empty scope (spec §13 step 1).
- Tests live in `crates/<crate>/tests/<name>_test.rs`. Avoid inline `#[cfg(test)] mod tests` in large source files.
- Test error paths, not just happy paths.
- Persistence changes: round-trip `views` (including unknown `ViewKind` preservation) and `undo.log` append/compact/replay. Use in-memory SQLite for cache tests.
- App changes: test undo/redo round-trips and `PhotoId` dedup before undo entries (spec decision #10).
- Performance is a feature: the 60 fps target is measured on the real painted egui grid, not a headless loop (spec decision #26). If a change risks the frame budget, say so.

## Workspace Hygiene

- All external deps live in root `Cargo.toml` `[workspace.dependencies]`. Crates pull only what they need via `dep.workspace = true`. One version per dependency across the workspace.
- Keep the crate DAG shallow and arrows-down. Changing `dcs-domain` recompiling everything is expected; changing `dcs-ui` recompiling `dcs-io` means the graph is wrong.

## Before Declaring Done

Run and confirm all pass:
```
cargo fmt
cargo build --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
```
If any fail, fix them. Do not tell the user "done" with a broken build.

## Task Completion

After each task, summarize:
1. **What was done** — behavior and design choices, not just file names.
2. **Why** — rationale for non-obvious decisions, alternatives considered.
3. **What to watch** — edge cases, known limitations, follow-up work.
4. **Tests added** — what's covered and what still needs integration/manual testing.

## When In Doubt

Check `spec.md` first — it's authoritative. If it doesn't cover the case, ask the user. Don't guess on architectural decisions.
