---
description: Critical concurrency and execution constraints for PostgreSQL
paths:
  - "pg_otel/**/*.rs"
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
