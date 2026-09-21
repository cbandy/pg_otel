---
status: accepted
date: 2026-09-21
deciders: ['@cbandy']
consulted: []
informed: []

---
# ADR 0007: Nested Statement Tracing and Budgeting Architecture

## Context and Problem Statement

`pg_otel` traces top-level client queries (DML, DDL, Utility) as `SpanKind::Server` spans. Under the re-entrancy design established in [ADR 0006](../0006-statement-tracing-and-guc-propagation/README.md), any nested execution (such as internal queries executed via SPI inside stored procedures, functions, procedural blocks, or triggers) is suppressed whenever `execution_depth > 0`.

While this suppression guarantees that client queries produce strictly $O(1)$ spans, it leaves [Database Developers](../../personas.md#database-developer) and [Query Authors](../../personas.md#application-developer) blind to the internal execution of stored procedures and cascading triggers. Stored procedures and triggers frequently account for significant database latency, yet external APMs see them only as a single opaque span.

However, naive auto-instrumentation of nested queries introduces a catastrophic operational hazard: **loop-driven span explosion**. A procedure containing `FOR rec IN SELECT ... LOOP` or a table with a `FOR EACH ROW` trigger executed on a batch update can easily issue tens or hundreds of thousands of SPI statements in a single query. Emitting spans for every iteration would saturate shared memory IPC buffers, degrade backend throughput, and overwhelm downstream OpenTelemetry collectors—violating the core availability invariants demanded by [Platform Engineers and DBAs](../../personas.md#platform-engineer).

How can `pg_otel` provide high-fidelity structural visibility into the internal statements of procedures and triggers while guaranteeing strict, predictable resource boundaries that protect database stability?

## Decision Drivers

- **Bounded Resource**: Telemetry generation must never cause unbounded memory allocation, network saturation, or backend throughput degradation, even in the presence of massive procedural loops or batch row triggers.
* **SQL Developer Mental Model**: In SQL and procedural database code, the primary cognitive unit of work is the **statement** (`CALL`, `UPDATE`, `INSERT`, `PERFORM`, `SELECT`, `MERGE`, `COMMIT`, `TRUNCATE`). Instrumentation should reflect this statement sequence rather than exposing low-level engine internals or row-level scalar expressions.
- **Safe-by-Default with Opt-In**: The default behavior must remain $O(1)$ top-level statement tracing. Opting into nested tracing should require zero code modifications to existing stored procedure or trigger bodies.
- **Minimal Hook Footprint**: Leverage the existing, battle-tested executor and utility hook architecture without introducing heavy or fragile function manager (FMGR) hooks.
- **OpenTelemetry Specification**: Seamlessly align with OpenTelemetry semantic conventions by modeling top-level client queries as `SpanKind::Server` and internal nested statements as `SpanKind::Internal`.

## Considered Options

- **Option 1: Unconditional Nested Tracing with Call-Depth Cutoff**
  Allow nested spans whenever `execution_depth <= max_depth` (e.g. max depth 3).
  *Critique*: Fails to solve the loop problem. A 100,000-iteration loop inside a stored procedure or a `FOR EACH ROW` trigger is only depth 2, yet it would still generate 100,000 spans.

- **Option 2: Pure SQL User-Space SDK (`otel.start_span` / `otel.end_span`)**
  Require developers to manually wrap blocks of procedural code with explicit function calls.
  *Critique*: Heavy operational friction. Developers must rewrite and redeploy stored procedures and triggers. PL/pgSQL lacks RAII / context managers, creating severe risks of leaked, unclosed spans during unhandled exceptions or early returns.

- **Option 3: Function Manager (FMGR) Routine-Level Tracing (`fmgr_hook`)**
  Intercept function entries and exits via `fmgr_hook`.
  *Critique*: Requires complex catalog lookups to filter out primitive C functions (`int4add`, `textcat`). More importantly, it creates spans for per-row scalar functions (e.g., `SELECT ST_Intersects(a, b) FROM big_table`), generating millions of leaf spans for row evaluations where distributed tracing is inappropriate. Furthermore, it misses raw SQL statements inside procedures that do not call sub-functions.

- **Option 4: Statement-Level Tracing with Two-Tier Budgeting and Delegated Sub-Budgets via Existing Executor Hooks (Chosen)**
  Trace nested SQL statements executed via SPI using existing `ExecutorStart`/`ExecutorEnd` and `ProcessUtility` hooks. Enforce a two-tier control model:
  1. A DBA-controlled administrative ceiling (`otel.max_nested_statements`, `SUSET`) that acts as an immutable kill-switch and cluster-wide upper bound.
  2. A user-configurable request budget (`otel.trace_nested_statements`, `USERSET`) that scopes tracing per session, transaction, or procedure.
  Within an active query tree, child procedures receive **delegated sub-budgets** clamped to the remaining parent budget (`min(parent_remaining, callee_guc)`), enabling routines to narrow or silence their telemetry without expanding beyond caller or DBA bounds.

## Decision Outcome

Chosen option: **Option 4 (Statement-Level Tracing with Two-Tier Budgeting and Delegated Sub-Budgets via Existing Executor Hooks)**.

### Consequences

- **Good**: **Zero-overhead, safe-by-default**. With default `otel.trace_nested_statements = 0`, execution skips nested span allocation on a fast-path integer check, preserving current $O(1)$ performance.
- **Good**: **Absolute DBA control & kill-switch**. Setting `otel.max_nested_statements = 0` guarantees no user or procedure can emit nested spans anywhere in the cluster.
- **Good**: **Bounded span volume**. The total child spans emitted across an entire query tree can never exceed `otel.max_nested_statements`, protecting shared memory and collectors from loop explosions.
- **Good**: **Caller-governed budget delegation**. The client or session context establishes the top-level budget (which can be defaulted via `ALTER ROLE`, `ALTER DATABASE`, etc). Stored procedures cannot unexpectedly expand telemetry overhead beyond what the caller granted.
- **Good**: **Hierarchical silencing and sub-budgeting**. A procedure setting `0` silences its own internals without burning caller budget; a procedure requesting a smaller budget preserves the remainder for its caller.
- **Good**: **No new hook dependencies**. All nested SQL statements pass through `ExecutorStart` and `ProcessUtility` via SPI, providing complete access to query text, operation names, and metrics using existing logic.
- **Good**: **Scalar expressions ignored**. Queries like `SELECT ST_Intersects(…) FROM big_table` execute zero sub-statements and emit exactly one `SELECT` span.
- **Bad**: Procedural control flow (`IF`, assignments, loops without SQL) is not visible as separate spans.

---

## Technical Specifications

### 1. Configuration Parameters

A two-tier parameter architecture balancing cluster administration with procedural flexibility:

- **`otel.max_nested_statements` (`PGC_SUSET` / `PGC_SIGHUP`, Default: `50`)**:
  - The immutable administrative ceiling established by DBAs in `postgresql.conf` or via `ALTER SYSTEM / ALTER DATABASE`.
  - Non-superusers and stored procedures **cannot** raise or override this ceiling.
  - Setting `otel.max_nested_statements = 0` acts as an absolute cluster-wide kill-switch: all nested statement tracing is suppressed, regardless of procedure declarations.
- **`otel.trace_nested_statements` (`PGC_USERSET`, Default: `0`)**:
  - The request / opt-in budget configured by application developers, connection poolers, roles, or databases.
  - `0` (default): Disables nested statement tracing for unconfigured statements.
  - `K > 0`: Requests a budget of up to $K$ nested child statement spans.

At query start, the effective budget is:
$$\text{REMAINING\_BUDGET} = \min(\text{otel.trace\_nested\_statements}, \text{otel.max\_nested\_statements})$$

### 2. Scoping and Ergonomics ("Manual Auto-Instrumentation")

Users and administrators can configure the client/session budget at multiple granularities using standard PostgreSQL mechanisms:

1. **Role or Database Defaults** (Persistent administration):
   ```sql
   ALTER ROLE app_user SET otel.trace_nested_statements = 25;
   ALTER DATABASE analytics_db SET otel.trace_nested_statements = 50;
   ```
2. **Client Connection Parameters** (Connection poolers / services):
   ```
   postgresql://user:pass@host/db?options=-c%20otel.trace_nested_statements=25
   ```
3. **Per-Transaction or Query Call in SQL**:
   ```sql
   BEGIN;
     SET LOCAL otel.trace_nested_statements = 50;
     CALL process_orders(101);
   COMMIT;
   ```
4. **Per-Request via SQLCommenter**:
   ```sql
   CALL process_orders(101) /* otel.trace_nested_statements=25 */;
   ```
5. **Procedure-Level Sub-Budgeting and Silencing**:
   Within an already-budgeted query, procedures can narrow their allowance or silence themselves:
   ```sql
   -- Cap process_orders to at most 5 child spans, preserving caller's remaining budget
   ALTER PROCEDURE process_orders SET otel.trace_nested_statements = 5;

   -- Completely silence noisy routine so it emits 0 spans and burns 0 caller budget
   ALTER PROCEDURE write_audit_log SET otel.trace_nested_statements = 0;
   ```

### 3. Execution Pipeline & State Machine (Delegated Sub-Budgets)

The active statement tracking replaces the single-slot `ACTIVE_STATEMENT_SPAN` with a thread-local stack supporting **delegated sub-budgets**:

```rust
struct ActiveStatement {
    span: Span,
    depth: usize,
    scope_budget: Cell<i32>,
}

thread_local! {
    static SPAN_STACK: RefCell<Vec<ActiveStatement>> = const { RefCell::new(Vec::new()) };
    static REMAINING_BUDGET: Cell<i32> = const { Cell::new(0) };
    static DROPPED_NESTED_STATEMENTS: Cell<usize> = const { Cell::new(0) };
}
```

#### Entry (`ExecutorStart_hook` and `ProcessUtility_hook`)
1. Increment `EXECUTION_DEPTH`.
2. **Top-Level Entry (`depth == 1`)**:
   - Initialize `REMAINING_BUDGET` from $\min(\text{otel.trace_nested_statements}, \text{otel.max_nested_statements})$.
   - Reset `DROPPED_NESTED_STATEMENTS` to `0`.
   - Start `SpanKind::Server` root statement span (parented by transaction span or client W3C context).
   - Push `ActiveStatement` with `scope_budget = REMAINING_BUDGET.get()`.
3. **Nested Entry (`depth > 1`)**:
   - Inspect parent scope: Let `parent = SPAN_STACK.last()`.
   - Compute delegated sub-budget:
     $$\text{child\_budget} = \min(\text{parent.scope\_budget.get()}, \text{REMAINING\_BUDGET.get()}, \text{otel.trace_nested_statements})$$
   - If `child_budget > 0`:
     - Deduct from parent scope budget: `parent.scope_budget.set(parent.scope_budget.get() - 1)`.
     - Deduct from query total: `REMAINING_BUDGET.set(REMAINING_BUDGET.get() - 1)`.
     - Create `SpanKind::Internal` statement span.
     - Set `parent_span_id` to current top of `SPAN_STACK`.
     - Inherit `trace_id` from root span.
     - If `MyTriggerDepth > 0`, attach attribute `db.postgresql.trigger = true`.
     - Push `ActiveStatement` to `SPAN_STACK` with `scope_budget = child_budget`.
   - Else (`child_budget == 0`):
     - Increment `DROPPED_NESTED_STATEMENTS`.
     - Return immediately (bypassing span creation).

#### Exit (`ExecutorEnd_hook` and `ProcessUtility_hook`)
1. Decrement `EXECUTION_DEPTH`.
2. If the top of `SPAN_STACK` matches the exiting depth:
   - Pop `ActiveStatement`.
   - If this was a nested statement (`depth > 1`), return any unused `scope_budget` back to the parent scope:
     `parent.scope_budget.set(parent.scope_budget.get() + popped.scope_budget.get())`.
   - If this is the top-level span (`depth == 0`) and `DROPPED_NESTED_STATEMENTS > 0`:
     - Attach attribute `otel.dropped_child_spans = DROPPED_NESTED_STATEMENTS`.
   - Record `end_time_unix_nano`.
   - Dispatch span to shared memory export queue.

### 4. Error Unwinding and Crash Safety

If an unhandled exception or abort occurs during nested execution (e.g. `elog(ERROR)` or caught `BEGIN ... EXCEPTION` blocks):
- PostgreSQL longjmps past intervening `ExecutorEnd` calls.
- `xact_callback(ABORT)` and `subxact_callback(ABORT_SUB)` inspect `SPAN_STACK`.
- Any dangling nested spans are popped, marked with `SpanStatus::Error` and the active `SQLSTATE`, given an end timestamp, and exported.
- Thread-local stack and depth counters are reset to `0`.

### 5. Native Logging as Span-Correlated Log Records

Per the OpenTelemetry specification, [OTEP 4430](https://opentelemetry.io/blog/2026/deprecating-span-events), events are formally modeled as log records correlated with the active trace and span context.

This architectural model is particularly advantageous for PostgreSQL:
- **Bounded In-Memory Span Footprint**: Storing events inside a `Span` requires accumulating an unbounded `Vec<SpanEvent>` during long-running procedures until `ExecutorEnd` fires. In contrast, log records are streamed independently through the existing OTLP log export pipeline.
- **Automatic Context Correlation**: When `emit_log_hook` intercepts messages (`RAISE NOTICE`, `RAISE WARNING`, `RAISE INFO`, or PL/Python `plpy.notice`), it reads the current statement's `(trace_id, span_id)` from the top of `SPAN_STACK`.
- **Standard OTLP Log Representation**: The exported `LogRecord` includes the active `trace_id` and `span_id`. Observability backends correlate these logs and display them inline on the span's waterfall execution timeline.

---

## Validation

1. **Unit & Integration Tests (`pgrx`)**:
   - Verify `CALL my_procedure()` with default `otel.trace_nested_statements = 0` emits exactly 1 span.
   - Verify caller governance: with client `otel.trace_nested_statements = 0`, `CALL my_procedure()` emits 0 nested spans even if `my_procedure` has `SET otel.trace_nested_statements = 10`.
   - Verify client opt-in: `CALL my_procedure() /* otel.trace_nested_statements=10 */` or `SET LOCAL` emits child spans for inner SPI queries with `SpanKind::Internal` and correct `parent_span_id` relationships.
   - Verify DBA kill-switch: with `otel.max_nested_statements = 0`, queries emit 0 nested spans even if client requests `otel.trace_nested_statements = 25`.
   - Verify delegated sub-budgeting: `outer_proc` (budget 20) calling `inner_proc` (budget 5) restricts `inner_proc` to 5 spans and preserves the remaining 15 for `outer_proc`.
   - Verify local silencing: `inner_proc` configured with `otel.trace_nested_statements = 0` emits 0 child spans and deducts 0 spans from `outer_proc`'s budget.
   - Verify caller clamping: `inner_proc` configured with budget 20 called from `outer_proc` with 5 remaining spans is clamped to 5 spans.
   - Verify loop protection: a 100-iteration loop calling an internal procedure never exceeds `otel.max_nested_statements`.
   - Verify `RAISE NOTICE` within a procedure is exported as an OTLP `LogRecord` correlated with the procedure's active `trace_id` and `span_id`.
   - Verify transaction abort cleanly unwinds and marks all open nested spans on `SPAN_STACK` as errors.
