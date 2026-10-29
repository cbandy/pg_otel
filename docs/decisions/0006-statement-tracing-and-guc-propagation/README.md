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
  Register `otel.traceparent` / `otel.tracestate` as PostgreSQL GUC parameters (`GUC_Userset`) with an `assign` hook and single-use `.take()` consumption.

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

`otel.traceparent` and `otel.tracestate` are registered as PostgreSQL GUC parameters the user can set at any time.
These do not behave entirely like regular parameters, so we must take care to make them intuitive. These will consistently follow a few principles:

#### Single-Use
Whenever `pg_otel` initializes a top-level statement or transaction span, it **clears** `otel.traceparent` and `otel.tracestate` to ensure they are used exactly once.
This prevents context from "leaking" to a subsequent statement, transaction, or connection pool.

#### Set Context (on or) Before
A user may assign W3C `traceparent` and `tracestate` context to a top-level statement in 2 ways:
- **SQLCommenter**: `SELECT … /* traceparent='00-1111…-2222…-01',tracestate='rojo=1' */;` — this is how auto-instrumentation or an ORM might behave
- **`SET` or `SET LOCAL` before**: `SET otel.traceparent = '00-1111…-2222…-01'; SET otel.tracestate = 'rojo=1'; SELECT …` — this is how a connection pooler might behave

Doing this sets the `trace_id`, `parent_id`, `flags`, and `trace_state` of the statement's span and attaches a **Span Link** back to the original parent span, if any.

#### First Context Affects the Transaction
The **first** context received for a transaction block sets the `TRANSACTION` span's identity and state. This first context can arrive in any of 3 ways:
- **SQLCommenter on `BEGIN`**: `BEGIN /* traceparent='00-3333…-4444…-01',tracestate='rojo=1' */;`
- **`SET` before `BEGIN`**: `SET otel.traceparent = '00-3333…-4444…-01'; SET otel.tracestate = 'rojo=1'; BEGIN`
- **`SET LOCAL` after `BEGIN`**: `BEGIN; SET LOCAL otel.traceparent = '00-3333…-4444…-01'; SET LOCAL otel.tracestate = 'rojo=1';`
  — this looks like an exception to the prior principle, but it's the natural way to affect a transaction in SQL

#### Immutable During Nested Execution (Caller Governance)
Spans produced by triggers, procedures, and function calls cannot (directly) affect the current context.
- Attempting to set `otel.traceparent` or `otel.tracestate` inside a routine body (including `ALTER FUNCTION/PROCEDURE … SET`) is rejected with an `ERROR`.
- Any SQLCommenter comments on nested statements are ignored.

This ensures internal spans correctly inherit `trace_id`, `trace_flags`, and `trace_state` from their enclosing span, and prevents procedural code from leaking GUC state.
This is stricter than described in [W3C Trace Context §3.4](https://www.w3.org/TR/trace-context#mutating-the-traceparent-field) and [§3.5](https://www.w3.org/TR/trace-context#mutating-the-tracestate-field).
We can relax this later without breaking existing PL routines.

#### Deferred: Outbound Context
It might be possible to return the current context using GUC hooks for `SHOW` and `current_setting()`, but for now `otel.traceparent` and `otel.tracestate` are single-use *input* parameters that clear when consumed.
- Returning a random span ID on every `SHOW` or `pg_settings` query might confuse existing tools that diff or examine GUCs.
- The `current_setting()` function is subtly inconsistent in PL/pgSQL: simple variable assignments `v := current_setting()` bypass executor hooks whereas `SELECT current_setting()` executes via nested SPI.

Exposing the current context so that outbound calls (FDWs, extensions) can propagate it is deferred for now.

### 4. Parallel Worker Propagation Architecture

1. **GUC Setup**: Whenever `qd.plannedstmt.parallelModeNeeded == true`, the leader backend sets `otel.traceparent` and `otel.tracestate` in `ExecutorStart_hook`.
2. **Automatic Inheritance**: PostgreSQL's native `SerializeGUCState()` / `RestoreGUCState()` carries these GUC into spawned parallel worker processes.
3. **Worker Execution**: Workers inspect `IsParallelWorker() == true` in `ExecutorStart_hook`, read those GUC, and emit worker child spans under the leader's active span.

