---
status: accepted
date: 2026-11-02
deciders: ['@cbandy']
consulted: []
informed: []

---
# ADR 0008: Multi-Statement Batch Transaction Spans and Boundary Detection

## Context and Problem Statement

In PostgreSQL's simple query protocol `'Q'`, a client can submit a single command string containing multiple SQL statements separated by semicolons (e.g. `SELECT 1; SELECT 2;` or `INSERT INTO t1 ...; INSERT INTO t2 ...;`).

PostgreSQL executes the entire multi-statement string within an **implicit transaction** (`exec_simple_query`):
- When `list_length(parsetree_list) > 1`, PostgreSQL invokes `BeginImplicitTransactionBlock()` which sets transaction state to `TBLOCK_IMPLICIT_INPROGRESS` before the first statement is analyzed, planned, or executed.
- All statements in the command string share the same transaction snapshot and transaction identifier.
- Atomicity is enforced: if any statement in the batch fails at parse, plan, or execution time, the entire batch is aborted and all changes roll back via `xact_callback(ABORT)`.
- When all statements complete successfully, PostgreSQL commits the batch as a single transaction via `EndImplicitTransactionBlock()` and `xact_callback(COMMIT)`.

Currently, `pg_otel` traces each statement in a multi-statement batch as an independent, unparented root span unless explicitly preceded by a `BEGIN` statement.
This causes fragmentation in distributed tracing systems:
1. **Disconnected Sibling Spans**: Operations that executed atomically within a single database transaction appear as detached, concurrent or disjoint operations with no structural relationship.
2. **Context Loss on Intermediate Errors**: When an intermediate statement fails before execution, the error span has no parent context linking it to the batch that triggered it.
3. **Trace Context Ambiguity**: SQLCommenter comments attached to the command string cannot cleanly scope their lifetime across the batch.

How can `pg_otel` emit a cohesive `TRANSACTION` span that parents all statements of a multi-statement batch, while avoiding overhead on queries that are single standalone statements?


## Decision Drivers

- **Engine Fidelity & Failure Domain**: Telemetry should accurately model the engine's execution and failure domain.
  Because PostgreSQL treats a multi-statement command string as an atomic transaction, the trace tree should reflect that hierarchy.
- **Zero Overhead on Single Queries**: Standalone queries (`SELECT 1`) must remain single top-level spans.
  We must not emit redundant `TRANSACTION` wrapper spans for single-statement queries.
- **Streaming Export (Zero Buffering)**: Individual statements must continue to export immediately upon completion to bound memory utilization and ensure low-latency export guarantees.
  We cannot buffer completed statement spans in memory until transaction commit.
- **Zero Text Parsing Overhead**: Multi-statement boundary detection should rely backend transaction state rather than custom SQL text scanning.
- **Clean Interaction with Explicit Transactions**: The system must handle explicit transaction commands (`BEGIN`, `COMMIT`, `ROLLBACK`) that appear at the start, middle, or across multiple protocol rounds of a session.
- **OpenTelemetry Semantic Conventions & Span Hierarchy**: Align with OpenTelemetry Database Span conventions, modeling all transactions (explicit and implicit) as `SpanKind::Server` with `db.operation.name = "TRANSACTION"` and child statements nested under them.


## Considered Options

### Option 1: Status Quo (Unparented Independent Spans)
Leave multi-statement batches as independent top-level statement spans without a parenting transaction.

*Critique*: Fails Decision Driver 1. Produces fragmented traces that misrepresent PostgreSQL's atomic failure domain in APM visualizations.

### Option 2: Post-Hoc Span Buffering (Defer Export Until Commit/Abort)
Do not emit spans as statements complete. Instead, buffer all completed statement spans in a thread-local queue until `xact_callback(COMMIT)` or `xact_callback(ABORT)` fires.
If the queue length is $\ge 2$, synthesize a `TRANSACTION` parent span, reparent the buffered statements, and export them all together.

*Critique*: Violates Decision Driver 3 (Streaming Export). Buffering statements introduces unbounded memory risks if a client sends hundreds or thousands of statements in a single batch, delaying span export and risking memory pressure.

### Option 3: Backend Transaction State Settlement via `IsTransactionBlock()` (Chosen)
Leverage PostgreSQL's internal transaction state machine.
In simple query mode, when the SQL message contains more than one statement, PostgreSQL calls `BeginImplicitTransactionBlock()` *before* the first statement executes.
At the first statement (`depth == 1`), `pg_otel` calls `IsTransactionBlock()`:
- For single statements outside a transaction, `IsTransactionBlock()` returns `false`.
- For multi-statement batches, `IsTransactionBlock()` returns `true`.
- When `IsTransactionBlock() == true` and no `TRANSACTION` span is currently on `SPAN_STACK`, open an implicit `TRANSACTION` span.

*Critique*: Zero text parsing overhead, 100% engine parity, and zero extra hooks required.

### Option 4: Query Buffer Lookahead Scanner at Statement Boundary
Inspect the SQL query string in `ExecutorStart_hook` or `ProcessUtility_hook`.
Using PostgreSQL's `stmt_location` and `stmt_len`, look ahead past the terminating semicolon to scan for subsequent non-comment tokens.

*Critique*: Requires a custom lightweight parser to skip whitespace, semicolons, and comments. Redundant given that PostgreSQL's C parser has already evaluated this.

### Option 5: Parser AST Hook Interception (`post_parse_analyze_hook`)
Intercept `post_parse_analyze_hook` to count statements.

*Critique*: The full parse tree is not visible in any global or static variable. This hook receives only a single query at a time, and is bypassed completely on syntax errors and cached plans.


## Decision Outcome

Chosen option: **Option 3 (Backend Transaction State Settlement via `IsTransactionBlock()`)**.

---

## Technical Specifications

### 1. During First Statement

When a statement enters `ExecutorStart_hook` or `ProcessUtility_hook` at `execution_depth == 1`:

```rust
let in_xact_block = unsafe { pg_sys::IsTransactionBlock() };
let has_active_xact_span = SPAN_STACK.with_borrow(|s| {
    s.iter().any(|entry| entry.is_transaction())
});

if is_begin_statement(pstmt, sql_str) {
    // Case 1: Explicit transaction start (BEGIN / START TRANSACTION)
    // Consumes any SQLCommenter comment attached to BEGIN as the explicit parent of the TRANSACTION.
    if !has_active_xact_span {
        start_transaction_span(pstmt, sql_str);
    }
} else if in_xact_block && !has_active_xact_span {
    // Case 2: Multi-statement implicit batch!
    // PostgreSQL entered TBLOCK_IMPLICIT_INPROGRESS because list_length(parsetree_list) > 1.
    // Generates fresh transaction IDs without consuming Statement 1's statement-level comment.
    start_transaction_span(None, None);
} else if in_xact_block && has_active_xact_span {
    // Case 3: Statement within an ongoing transaction (from an earlier BEGIN or batch start).
    // Statement automatically parents to the active TRANSACTION span.
} else {
    // Case 4: Standalone single statement (s->blockState == TBLOCK_STARTED).
    // No transaction span needed; statement span runs as root.
}
```

### 2. Handling Explicit Transaction Commands Within Batches

PostgreSQL permits explicit transaction control commands within simple query batches:

1. **`BEGIN` at Batch Start**: `BEGIN; SELECT 1; COMMIT`
   Statement matches Case 1 and opens an explicit `TRANSACTION` span. Subsequent statements parent to it.
2. **`BEGIN` Mid-Batch**: `SELECT 1; BEGIN; SELECT 2; COMMIT`
   PostgreSQL natively allows this: in `xact.c`, `BEGIN` converts *implicit* into *explicit*.
   In `pg_otel`, the first statement opened an implicit `TRANSACTION` span under Case 2.
   When `BEGIN` executes, `has_active_xact_span` is `true`, so `pg_otel` promotes the existing transaction span to explicit without spawning a duplicate.
3. **`COMMIT` / `ROLLBACK` Mid-Batch**: `SELECT 1; COMMIT; SELECT 2`
   When `COMMIT` executes midway, PostgreSQL commits the first transaction and triggers `xact_callback(COMMIT)`, closing the first `TRANSACTION` span.
   When the next statement executes, `exec_simple_query` calls `BeginImplicitTransactionBlock()` again, restarting an implicit transaction block.
   `pg_otel`'s Case 2 triggers again, opening a fresh `TRANSACTION` span for the second half of the batch.

### 3. Lifecycle and Stack Coordination

```mermaid
sequenceDiagram
    autonumber
    participant Client
    participant Hooks as pg_otel Hooks
    participant Engine as PostgreSQL Engine
    participant Stack as SPAN_STACK
    participant Exp as Exporter Queue

    Client->>Engine: simple_query("SELECT 1; SELECT 2")
    Note over Engine: parsetree_list.len == 2 > 1
    Engine->>Engine: BeginImplicitTransactionBlock() (TBLOCK_IMPLICIT_INPROGRESS)

    Engine->>Hooks: ExecutorStart(Statement 1)
    Hooks->>Engine: IsTransactionBlock() == true
    Hooks->>Stack: has_active_xact_span == false -> push(TRANSACTION)
    Hooks->>Stack: push(Statement 1)
    Note over Stack: SPAN_STACK: [TRANSACTION, Statement 1]

    Engine->>Hooks: ExecutorEnd(Statement 1)
    Hooks->>Stack: pop(Statement 1)
    Hooks->>Exp: finish_and_export(Statement 1)
    Note over Stack: SPAN_STACK: [TRANSACTION] (persists!)

    Engine->>Hooks: ExecutorStart(Statement 2)
    Hooks->>Stack: has_active_xact_span == true -> inherit parent
    Hooks->>Stack: push(Statement 2)
    Note over Stack: SPAN_STACK: [TRANSACTION, Statement 2]

    Engine->>Hooks: ExecutorEnd(Statement 2)
    Hooks->>Stack: pop(Statement 2)
    Hooks->>Exp: finish_and_export(Statement 2)
    Note over Stack: SPAN_STACK: [TRANSACTION]

    Engine->>Engine: EndImplicitTransactionBlock() -> finish_xact_command()
    Engine->>Hooks: xact_callback(COMMIT)
    Hooks->>Stack: pop(TRANSACTION)
    Hooks->>Exp: finish_and_export(TRANSACTION)
    Note over Stack: SPAN_STACK: []
```

### 4. Span Properties and Attributes

To maintain symmetry with explicit transactions (where `BEGIN` is absorbed directly into the `TRANSACTION` span rather than emitted as an independent child statement span), implicit batch transactions use `SpanKind::Server` as the overarching parent span:

- **Span Name**: `"TRANSACTION"`
- **Span Kind**: `SpanKind::Server`
- **Standard Attributes**:
  - `db.operation.name`: `"TRANSACTION"`
  - `db.system.name`: `"postgresql"`

If an operational need arises to distinguish engine-synthesized batch transactions from explicit `BEGIN` transactions, attributes should be used rather than altering `SpanKind` or span naming.

### 5. Context and SQLCommenter Precedence

In a multi-statement batch, SQLCommenter comments attach to individual statements, not to the batch as a whole.

- **Fresh Transaction IDs for Implicit Batches**: The synthesized `TRANSACTION` span generates its own fresh `TraceId` and `SpanId`.
  It does not steal or consume a comment attached to the first statement.
- **Statement Context with Span Link**: When a statement in the batch carries a SQLCommenter comment, it adopts that comment as its explicit parent and creates an OpenTelemetry **Span Link** to the enclosing `TRANSACTION` span.
- **Default Inheritance**: Statements without their own SQLCommenter comments inherit the `trace_id` of the enclosing `TRANSACTION` span and set `parent_span_id = transaction.span_id`.
- **Explicit Parent on Transactions**: If a client intends to set an explicit parent directly on a `TRANSACTION` span, they can use an explicit transaction, where the comment is parsed on `BEGIN`.

### 6. Error and Abort Propagation

If an intermediate statement fails: `SELECT 1; SELECT 1/0; SELECT 2`
1. The first statement finishes and exports `status: OK`.
2. The second statement fails planning or execution. An error statement span is emitted `status: Error`.
3. PostgreSQL triggers `xact_callback(ABORT)`.
4. `pg_otel` updates the `TRANSACTION` span with `status: Error` and exports it.
5. In the APM trace view, the batch is presented with full structural fidelity:
   ```text
   TRANSACTION (Error: 22012)
     ├── SELECT 1 (OK)
     └── SELECT 1/0 (Error: 22012)
   ```

---

## Pros and Cons of the Options

### Option 3: Backend Transaction State Settlement via `IsTransactionBlock()` (Chosen)

* **Good, because zero string parsing overhead**: Does not inspect or parse the raw SQL string for semicolons, trailing comments, or whitespace.
* **Good, because 100% engine parity**: Relies directly on PostgreSQL's own parser decision (`list_length(parsetree_list) > 1`) and transaction state machine.
* **Good, because zero span bloat on single queries**: Standalone queries run with `IsTransactionBlock() == false`.
* **Good, because streaming and zero-buffering**: Preserves $O(1)$ memory usage. Spans export as statements complete.
* **Good, because zero new PostgreSQL hooks**: Implemented entirely within existing `ExecutorStart`, `ProcessUtility`, and `xact_callback` hooks.
* **Good, because handles complex multi-statement batches**: Correctly adapts when `BEGIN` or `COMMIT` occurs mid-batch.

### Option 4: Query Buffer Lookahead Scanner

* **Neutral, because accurate**: Can determine multi-statement status by inspecting the query buffer past the first statement.
* **Bad, because parser duplication**: Duplicates parsing logic to handle whitespace, block comments, and single-line comments, which PostgreSQL has already parsed.
* **Bad, because edge-case risks**: Non-standard or malformed comments could lead to misclassification.

---

## Validation Plan

1. **Integration Tests in `pg_otel/src/tracing/tests/transactions.rs`**:
   - `simple_query_batch_two_statements`: `SELECT 1; SELECT 2` emits a `TRANSACTION` parent span and two child `SELECT` spans.
   - `standalone_single_statement`: `SELECT 1` emits only one `SELECT` span (no `TRANSACTION` wrapper).
   - `batch_with_mid_batch_begin`: `SELECT 1; BEGIN; SELECT 2; COMMIT` emits single overarching `TRANSACTION` span.
   - `batch_with_mid_batch_commit`: `SELECT 1; COMMIT; SELECT 2` emits two sequential `TRANSACTION` spans.
   - `batch_failure_reparents_error`: `SELECT 1; SELECT 1/0; SELECT 2` marks `TRANSACTION` span as failed.
2. **Cross-Version Verification**:
   - Verified via `cargo pgrx test` across all supported PostgreSQL versions.
