---
status: accepted
date: 2026-09-02
deciders: ['@cbandy']
consulted: []
informed: []

---
# ADR 0006: Statement Tracing Architecture, Minimal Hook Model, and Dual-Purpose GUC Propagation

## Context and Problem Statement

`pg_otel` requires a clean, robust, and performant statement-level OpenTelemetry distributed tracing implementation across all supported PostgreSQL versions (PG13 through PG19).

## Decision Drivers

* **Minimal Complexity & Determinism**: Use the smallest possible set of PostgreSQL hooks to reliably cover 100% of user queries (DML, DDL, Utility, Transactions, Procedural blocks).
* **Process & Re-Entrancy Safety**: Guard nested subqueries and SPI executions from emitting duplicate top-level statement spans.
* **Parallel Query Trace Integrity**: Ensure parallel query worker processes inherit the leader's trace context without trace fragmentation or session pollution.
* **ORM & Tooling Ergonomics**: Support W3C `traceparent` context injection from application clients, connection poolers, and SQLCommenter comments.
* **Zero Session Pollution**: Guarantee that `traceparent` context set for one statement or transaction never leaks to subsequent unrelated queries on persistent pooled connections.

## Considered Options

### Hook Model Options:
* **Option A1: 4-Hook Minimal Foundation (Chosen)**
  Use strictly `ProcessUtility_hook`, `ExecutorStart_hook`, `ExecutorEnd_hook`, and `xact_callback(ABORT / PARALLEL_ABORT)`.
* **Option A2: Legacy Hook Set (`ExecutorFinish_hook` + `planner_hook` + `post_parse_analyze_hook`)**
  Use `ExecutorFinish` for completion and hook into planner/parse steps.

### Parallel Worker Propagation Options:
* **Option B1: Custom Fixed Shared Memory Array**
  Allocate `[ParallelSlot; MaxBackends]` in Postmaster shared memory during startup.
* **Option B2: Dual-Purpose GUC Inheritance with Single-Use `.take()` Auto-Clearing (Chosen)**
  Register `pg_otel.traceparent` / `pg_otel.tracestate` as PostgreSQL GUC parameters (`GUC_Userset`) with an `assign` hook and single-use `.take()` consumption.

## Decision Outcome

Chosen options: **Option A1 (4-Hook Minimal Foundation)** and **Option B2 (Dual-Purpose GUC Inheritance with `.take()` Auto-Clearing)**.

---

## Technical Specifications

### 1. Minimal Hook Foundation

1. **`ProcessUtility_hook`**: Bounds non-DML utility statements, DDL, transaction control (`BEGIN`, `COMMIT`, `ROLLBACK`), cursors, and procedural `DO` blocks (`execution_depth == 0`).
2. **`ExecutorStart_hook`**: Bounds direct DML queries (`SELECT`, `INSERT`, `UPDATE`, `DELETE`), SPI executions, and parallel query execution workers (`IsParallelWorker() == true`).
3. **`ExecutorEnd_hook`**: Exit boundary for top-level DML query pipelines.
4. **`xact_callback`** (`XACT_EVENT_ABORT` / `XACT_EVENT_PARALLEL_ABORT`): Safety net to mark active spans with `StatusCode::Error`, export remaining active spans, and reset thread-local `execution_depth` to `0`.

### 2. Re-Entrancy

- A thread-local cell `execution_depth: Cell<usize>` tracks execution nesting depth.
- **Top-Level Span Entry (`depth == 0`)**: Initiated only when `execution_depth == 0` during `ProcessUtility_hook` or `ExecutorStart_hook` (in non-worker backends).
- **Sub-Operation Guard (`depth > 0`)**: Calls while `execution_depth > 0` are tracked as child execution steps.
- **Top-Level Span Exit (`depth == 0`)**: Decrements `execution_depth`. When returning to `0`, the active top-level statement span is finalized and dispatched to the exporter queue.

### 3. Dual-Purpose GUC Interface

`pg_otel.traceparent` and `pg_otel.tracestate` are registered as PostgreSQL GUC parameters the user can set at any time.
These do not behave entirely like regular parameters, so we must take care to make them intuitive. These will consistently follow a few principles:

#### Single-Use
Whenever `pg_otel` initializes a span, it **clears** `pg_otel.traceparent` to ensure it is used exactly once.
This prevents the `traceparent` context from "leaking" to a subsequent statement, transaction, or connection pool.

#### Set Context (on or) Before
A user may assign a `traceparent` context to a statement in 2 ways:
* **SQLCommenter**: `SELECT … /* traceparent='00-1111…-2222…-01' */;` — this is how auto-instrumentation or an ORM might behave
* **`SET` or `SET LOCAL` before**: `SET pg_otel.traceparent = '00-1111…-2222…-01''; SELECT …` — this is how a connection pooler might behave

Doing this sets the `trace_id` and `parent_id` of the statement's span and attaches a **Span Link** back to the original parent span, if any.

#### First Context Affects the Transaction
The **first** `traceparent` context received for a transaction block sets the `TRANSACTION` span's identity. This first context can arrive in any of 3 ways:
* **SQLCommenter on `BEGIN`**: `BEGIN /* traceparent='00-3333…-4444…-01' */;`
* **`SET` before `BEGIN`**: `SET pg_otel.traceparent = '00-3333…-4444…-01'; BEGIN`
* **`SET LOCAL` after `BEGIN`**: `BEGIN; SET LOCAL pg_otel.traceparent = '00-3333…-4444…-01';`
  — this looks like an exception to the prior principle, but it's the natural way to affect a transaction in SQL

### 4. Parallel Worker Propagation Architecture

1. **GUC Setup**: When `qd.plannedstmt.parallelModeNeeded == true`, the leader backend sets `pg_otel.traceparent = <statement_span_context>` in `ExecutorStart_hook`.
2. **Automatic Inheritance**: PostgreSQL's native `SerializeGUCState()` / `RestoreGUCState()` carries `pg_otel.traceparent` into spawned parallel worker processes.
3. **Worker Execution**: Workers inspect `IsParallelWorker() == true` in `ExecutorStart_hook`, read `pg_otel.traceparent` via `.take()`, emit worker child spans under the leader's trace, and clean up automatically upon completion.

