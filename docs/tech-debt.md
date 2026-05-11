# Technical Debt

> This document tracks known issues in the Opal codebase. Each entry is tagged by priority and area. Resolve an item by removing it, not by adding a "Fixed" label.

---

## Async / Sync Anti-pattern

**Priority: High**

### Ad-hoc `tokio::runtime::Runtime::new()` + `block_on()` proliferation

Every call site that needs to call an `async fn` from non-async code creates its own Tokio runtime with `tokio::runtime::Runtime::new()`, then calls `block_on()`. This pattern appears **20+ times** across the codebase:

| File | Lines | Context |
|------|-------|---------|
| `mcp/tools.rs` | ~20+ sites | Each MCP tool handler creates a fresh runtime |
| `app/run.rs` | 339, 357 | Test code |
| `model/lowering.rs` | 16-17 | Sync wrapper blocks on async variant |

**Impact:**
- **Runtime overhead** — each `Runtime::new()` allocates a full thread pool (~cores threads). Repeated creation/teardown per tool call is wasteful.
- **Thread pool exhaustion** — if these runtimes are alive concurrently (e.g., parallel tests), the system spawns dozens of threads.
- **Re-entrancy risk** — calling `block_on` from within an existing Tokio runtime panics (single-threaded) or degrades (multi-threaded).
- **No shared state** — can't share `tokio::Handle`, background tasks, or cancellation across isolated runtimes.
- **Graceful shutdown** is impossible — each runtime is fire-and-forget.

**Fix:**
1. Own a single runtime at the app level (already done via `#[tokio::main]` in `bin/opal.rs`).
2. Pass a `tokio::runtime::Handle` (or an async executor trait) down to MCP tools and other callers instead of `Runtime::new()`.
3. Remove sync wrappers or have them use the shared handle.
4. Keep `spawn_blocking` for actual lock-holding work (`resource_groups.rs`) as the exception.

### Inconsistent async boundaries

Three `spawn_blocking` calls exist in the orchestrator:

| File | Line | What |
|------|------|------|
| `executor/orchestrator.rs` | 122 | Blocking work from async context |
| `executor/orchestrator/resource_groups.rs` | 100, 108 | `try_acquire` / `release` (lock-holding, justified) |

The resource group calls are correct (wrapping mutex operations), but the ad-hoc runtime pattern elsewhere makes the overall async boundary unclear.

---

## Nested Loop Spaghetti

**Priority: High**

### `compiler/compile.rs` — 4 TODOs

Triple-nested for loops throughout the compile pipeline, self-identified as needing restructuring:

| Line | Function | Issue |
|------|----------|-------|
| 67 | `compile_pipeline` | stage → job → variant loops, needs splitting |
| 247 | `select_variants` | Brittle nested filter/any/all chain |
| 308 | `expand_job_variants` | Matrix expansion triples loop |
| 355 | `matrix_combinations` | entry → variable → combo → value, 4 levels deep |

**Impact:** Hard to read, hard to test individual pieces, error messages lose context when deep nesting masks the source of failure.

### `model/lowering.rs`

| Line | Issue |
|------|-------|
| 40 | Nested loops converting `PipelineGraph` → `PipelineSpec` |

### `pipeline/mounts.rs`

| Line | Issue |
|------|-------|
| 55 | `collect_volume_mounts` does too much — dependency resolution, variant iteration, and artifact mount collection all in one function |

### Other

| File | Line | Issue |
|------|------|-------|
| `executor/core.rs` | 389 | 50 lines inside a `.map()` closure — panics lose error context |
| `ui/runner.rs` | 126 | `draw()` does too much — layout, rendering, state updates all mixed |
| `ui/runner.rs` | 272 | Same function does way too much, separate concerns needed |
| `execution_plan/types.rs` | 138 | Nth function following the same repetitive pattern |
| `compiler/instances.rs` | 50 | Nested functions with filters within filters |
| `ui/mod.rs` | 19 | "DO NOT ADD CODE" marker — module should be thin |

---

## Module Architecture

**Priority: Medium**

### `pipeline/rules.rs` — scattered logic

The module (942 lines) self-identifies: logic is split between structs and free functions without clear ownership. Should be consolidated into a proper rules engine trait with clear interfaces.

### Repeated `JobSpec` fixture construction

The `JobSpec` struct has ~30 fields. Every test file reconstructs it with full field listings (see `compile.rs:620`, `mounts.rs:542`). If the struct gains fields, all tests break. A fixture builder or test helper crate would reduce this maintenance burden.

---

## Unsafe Code

**Priority: Medium**

### `config.rs` — 6 unsafe blocks

Environment variable manipulation (`env::set_var`, `env::remove_var`) in tests uses `unsafe` blocks around non-atomic env operations. These work but make test ordering fragile.

### `mcp/tools.rs` — 48 unsafe blocks

The highest count. Most are `CString::from_raw`, `Box::from_raw`, or FFI bridge operations for the MCP JSON-RPC protocol. These are likely correct but undocumented — each should have a safety comment explaining why the operation is sound.

### `mcp/resources.rs` — 4 unsafe blocks

Same FFI pattern as tools.

### `app/run.rs` — 2 unsafe, `app/view.rs` — 4 unsafe

Environment variable cleanup in test code.

---

## Hardcoded Values

**Priority: Low**

### `executor/container.rs`

| Line | Issue |
|------|-------|
| 89 | Hardcoded timeout — should be a configurable default, not inline |

### `pipeline/cache.rs`

| Line | Issue |
|------|-------|
| 364 | Variable expansion happening inside cache logic — unexpected coupling |

---

## Clippy Suppressions

**Priority: Low**

### `ui/runner.rs`

| Line | Issue |
|------|-------|
| 43 | Skip clippy macros — track why and remove suppression when fixable |
