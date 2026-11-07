---
status: accepted
date: 2026-07-16
deciders: ['@cbandy']
consulted: []
informed: []

---
# Rust PGRX Prototype for pg_otel Extension

## Context and Problem Statement

We want to prototype a Rust-based implementation of the `pg_otel` PostgreSQL extension using `pgrx`.
The current C extension intercepts events (like logs) in backend database sessions and sends them over a UNIX pipe (`pipe(2)`) to a background worker process, which batches and exports them to an OpenTelemetry collector.
The key goals of the Rust prototype are:
1. Leverage Rust's memory safety and rich OpenTelemetry crate ecosystem.
2. Maintain the same design: Hooks in Postmaster/backends send serialized messages over a pipe to a dedicated background worker.
3. Absolutely **no threading** in the hooks/backends (to avoid issues with Postgres' process-forking model).
4. Sparse/minimal threading in the background worker.

What choices should we make regarding hook integration, pipe serialization, and the background worker runtime in this prototype?

## Decision Drivers

*   **Process Safety**: Spawning threads in backends is unsafe due to `fork()`.
*   **Performance**: Overhead in backends (hooks) must be minimal to avoid slowing down query processing.
*   **Maintainability & Ergonomics**: Leverage `pgrx` and the Rust crate ecosystem rather than writing raw unsafe bindings where possible.
*   **Robustness**: Handling buffers, chunks, and network exporter failures gracefully.

## Considered Options

### 1. Hook Integration
*   **Option A**: Use raw unsafe C bindings to hook functions like `emit_log_hook` manually.
*   **Option B**: Use `pgrx` high-level hook wrappers if available, falling back to safe abstractions around standard hooks.

### 2. IPC Protocol
*   **Option A**: Match the C implementation's syslogger-like chunking protocol exactly (for eventual compatibility or simplicity of transition).
*   **Option B**: Use a new Rust-native serialization and framing format (e.g., length-prefixed postcard/bincode over the pipe).

### 3. Background Worker Runtime
*   **Option A**: Single-threaded async runtime (e.g., `tokio` with `current_thread` scheduler).
*   **Option B**: Fully synchronous worker using blocking network calls (e.g., `reqwest::blocking` or `ureq`).
*   **Option C**: Multi-threaded async runtime (e.g., standard `tokio` work-stealing scheduler) - *strongly discouraged due to user's threading constraints*.

## Decision Outcome

Chosen options:
1. **Hook Integration**: We will implement hook registration entirely in Rust using `pgrx`. For hooks that `pgrx` does not natively wrap (such as `emit_log_hook`), we define raw/unsafe hook bindings directly.
2. **IPC Protocol**: Evaluated length-prefixed framing over pipes, but subsequently superseded by [ADR 0004](../0004-shared-memory-mpsc-queue/README.md) which adopted a lock-free shared-memory MPSC ring buffer for zero-syscall overhead and native cross-platform support.
3. **Background Worker Runtime**: Implemented as a synchronous worker using blocking network calls (`reqwest::blocking`, Option B), avoiding async runtime footprint inside Postgres background worker processes.

### Consequences

*   **Good**: Native integration with the Rust OpenTelemetry ecosystem (`prost`, `opentelemetry-proto`, `reqwest`).
*   **Good**: Complete safety against backend forks by avoiding background OS threads in both hooks/backends and the background worker.
*   **Neutral**: Implementing raw hook registration requires using `unsafe` blocks in Rust to override global hook pointers (like `emit_log_hook`).

---

## Outcome: Full Adoption & Decommissioning of C Implementation

The Rust implementation has demonstrated superior memory safety, performance, maintainability, and feature capabilities (including distributed tracing, nested statement tracing in [ADR 0006](../0006-statement-tracing-and-guc-propagation/README.md) and [ADR 0007](../0007-nested-statement-tracing-and-budgeting/README.md), and shared memory IPC in [ADR 0004](../0004-shared-memory-mpsc-queue/README.md)).

As a result:
* The prototype is officially accepted as the permanent implementation of `pg_otel`.
* All legacy C implementation files, PGXS Makefiles, and Perl/Docker test suites are decommissioned and removed.
* Supported PostgreSQL versions are standardized to **PostgreSQL 15 through 19** via `pgrx`, officially retiring legacy PostgreSQL 11–14 support for `pg_otel`.
  *(Note: `sequencing` retains tests for PostgreSQL 13 and 14 for historical reference).*
