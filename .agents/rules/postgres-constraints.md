---
description: Critical concurrency and execution constraints for PostgreSQL
globs:  "pg_otel/**/*.rs"
paths: ["pg_otel/**/*.rs"]
trigger: glob
---

# PostgreSQL Process & Concurrency Constraints

When generating, modifying, or refactoring code for `pg_otel`, you must strictly adhere to the following PostgreSQL architectural invariants:

## 1. No Threading in Client Backends or Hooks
- **Constraint**: Never spawn threads or instantiate multi-threaded async runtimes inside backend hooks or code.
- **Why**: PostgreSQL backends are single-threaded processes that are `fork()`ed from Postmaster and call `fork()` themselves.
- **Rule**:
  - **DO**: Run backend hook code synchronously and return quickly.
  - **DON'T**: Never spin up background threads or async thread pools within a backend process.

## 2. Strictly Non-Blocking Hooks
- **Constraint**: Emitting telemetry from backend hooks into the IPC channel must be non-blocking.
- **Why**: Telemetry instrumentation must never degrade database client performance or stall transactional queries.
- **Rule**:
  - **DO**: Use non-blocking enqueue operations. If the queue/channel is full, record a drop counter or gracefully discard the message.
  - **DON'T**: Never use blocking mutexes, condition variables, unbounded retry loops, or blocking I/O inside hooks.

## 3. Single-Threaded Background Worker
- **Constraint**: The telemetry exporter background worker (`bgworker`) should favor a single-threaded event loop (e.g., single-threaded async executor or non-blocking polling).
- **Why**: Minimizes memory footprint and avoids process-lifecycle complexities in PostgreSQL's worker model.
- **Rule**: Only introduce auxiliary threads in the background worker if strictly necessary for foreign library integrations, and never in code paths shared with backend hooks.

## 4. FFI Pointer Safety & Alignment in Hooks
- **Constraint**: Do not check `ptr.is_aligned()` before `unsafe { ptr.as_ref() }` on typed hook arguments (Postgres guarantees `MAXALIGN` by construction).
- **Rule**:
  - **DO**: Use `if let Some(v) = unsafe { ptr.as_ref() }` directly on typed hook arguments.
  - **DON'T**: Do not add defensive `ptr.is_aligned()` checks on typed C arguments.
  - **EXCEPTION**: Pointer arithmetic over byte buffers (e.g., `shmem.rs`) MUST enforce or validate alignment.

## 5. FFI Aliasing Rules Across Foreign C Hook Chains
- **Constraint**: Never hold Rust references (`&T` or `&mut T`) across calls to foreign C hook chains (`call_remaining_hooks()` or `standard_*`).
- **Why**: In Rust, an immutable reference `&T` asserts to LLVM that no pointer alias can mutate the memory. Foreign C hooks frequently mutate passed structures (such as `ErrorData` or `PlannedStmt`). Holding a reference across foreign calls causes undefined behavior when downstream C hooks mutate the struct.
- **Rule**:
  - **DO**: Scope pre-chain borrows into temporary blocks, drop all references before invoking foreign C functions, and re-borrow from raw pointers (`as_ref()`) afterwards.
  - **DON'T**: Never pass a raw pointer to downstream C hooks while keeping an active Rust reference to the same memory.
