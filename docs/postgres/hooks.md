# PostgreSQL Extension Hook Catalog

This document provides a comprehensive catalog of all hooks and callback registries available to a dynamic shared-library extension in PostgreSQL. It covers symbol locations, function signatures, process execution contexts, standard chaining conventions, and their telemetry relevance for `pg_otel`.

---

## Hook Chaining Convention

Unlike callback registries that maintain a linked list of subscribers, most PostgreSQL hooks are exposed as mutable global function pointers.

To allow multiple extensions to coexist without overwriting each other, extensions **must** follow the standard chaining pattern:

```c
/* Static variable to preserve previously installed hook */
static ExecutorStart_hook_type prev_ExecutorStart_hook = NULL;

static void
my_ExecutorStart_hook(QueryDesc *queryDesc, int eflags)
{
    /* 1. Pre-execution telemetry */

    /* 2. Chain to previous hook if present; otherwise call standard fallback */
    if (prev_ExecutorStart_hook)
        prev_ExecutorStart_hook(queryDesc, eflags);
    else
        standard_ExecutorStart(queryDesc, eflags);

    /* 3. Post-execution telemetry */
}

void
_PG_init(void)
{
    /* Save existing hook and install our custom hook */
    prev_ExecutorStart_hook = ExecutorStart_hook;
    ExecutorStart_hook = my_ExecutorStart_hook;
}
```

> [!IMPORTANT]
> Failure to call the previous hook breaks other loaded extensions. Always preserve and invoke `prev_*_hook` if non-NULL, or invoke the `standard_*` fallback function if NULL.

---

## Server Lifecycle & Shared Memory

| Hook / Entrypoint | Invocation & Interception | Process Context | Telemetry Relevance |
| :--- | :--- | :--- | :--- |
| **`_PG_init`** | Called immediately when the shared library is loaded. | Postmaster (startup) & Backends (on-demand) | Define custom GUCs, register background workers (`RegisterBackgroundWorker`), and install hook function pointers. |
| **`shmem_request_hook`** *(PG15+)* | Executed during shared memory size estimation, before OS allocation. | Postmaster | Request shared memory bytes (`RequestAddinShmemSpace`) and reserve named LWLocks (`RequestNamedLWLockTranche`). |
| **`shmem_startup_hook`** | Executed after shared memory segment allocation and LWLock tranche creation. | Postmaster (and single-user backend) | Initialize shared memory segments (`ShmemInitStruct`) and bind IPC queue pointers before workers or backends fork. |

---

## Connection & Client Authentication

These hooks execute during the initial connection handshake before PostgreSQL enters the interactive query loop.

| Hook | Invocation & Interception | Process Context | Telemetry Relevance |
| :--- | :--- | :--- | :--- |
| **`ClientAuthentication_hook`** | Called at the conclusion of client authentication; receives connection port and authentication status. | Client Backend | Spans for connection establishment, recording TLS protocol/cipher, user role, client IP, database name, and authentication failure events. |
| **`check_password_hook`** | Intercepts password creation and modification commands; receives plaintext and hashed credentials. | Client Backend | Auditing password change operations and enforcement of credential complexity policies. |
| **`ldap_password_hook`** | Intercepts password resolution during LDAP authentication. | Client Backend | LDAP credential mutator hook. |
| **`openssl_tls_init_hook`** *(PG17+)* | Called during SSL/TLS context initialization; receives the OpenSSL context pointer. | Postmaster & Client Backend | Custom OpenSSL context initialization, cipher suite enforcement, and TLS handshake observability. |

---

## Parsing & Semantic Analysis

| Hook | Invocation & Interception | Process Context | Telemetry Relevance |
| :--- | :--- | :--- | :--- |
| **`post_parse_analyze_hook`** | Called immediately after parse analysis converts raw SQL AST into a query tree; receives parse state and query tree. | Client Backend | Measures parse/analyze duration, extracts query trees, and computes or consumes query identifiers identical to `pg_stat_statements`. |

---

## Query Planning & Optimization

| Hook | Invocation & Interception | Process Context | Telemetry Relevance |
| :--- | :--- | :--- | :--- |
| **`planner_hook`** | Wraps standard query planning; receives the parse tree, query string, cursor options, and bound parameters. | Client Backend | Measures query planner duration, tracks optimizer overhead vs execution time, and attaches planning spans. |
| **`planner_setup_hook`** *(PG19+)* | Called after `PlannerGlobal` is initialized, before subquery and relation planning begins. | Client Backend | Plugin control and planner state setup. |
| **`planner_shutdown_hook`** *(PG19+)* | Called after planning concludes, right before `PlannerGlobal` is discarded. | Client Backend | Plugin cleanup and planner resource teardown. |
| **`set_rel_pathlist_hook`** | Called when generating access paths for a base table or foreign scan. | Client Backend | Custom scan provider path addition and per-table access path instrumentation. |
| **`set_join_pathlist_hook`** | Called when generating join paths between pairs of relations. | Client Backend | Custom join implementation and join cost instrumentation. |
| **`join_path_setup_hook`** *(PG19+)* | Called during join path setup for a join relation. | Client Backend | Plugin control during `set_rel_pathlist()` for join paths. |
| **`join_search_hook`** | Replaces standard join tree search (GEQO or dynamic programming). | Client Backend | Alternative join order search algorithms. |
| **`joinrel_setup_hook`** *(PG19+)* | Called during join relation construction. | Client Backend | Plugin control during join relation setup. |
| **`get_relation_info_hook`** *(PG13–PG18)* | Called when fetching table metadata and index definitions during planning. | Client Backend | Intercepts table/index metadata lookup (replaced by `build_simple_rel_hook` in PG19). |
| **`build_simple_rel_hook`** *(PG19+)* | Called when building `RelOptInfo` for a simple relation. | Client Backend | Intercepts base relation initialization in the planner. |
| **`create_upper_paths_hook`** | Called when planning grouping, aggregation, window functions, and distinct stages. | Client Backend | Instruments post-scan/join stages (aggregation, window functions, distinct, sorting). |
| **`get_relation_stats_hook`** | Intercepts relation selectivity and cardinality lookups. | Client Backend | Overrides relation selectivity and cardinality estimates. |
| **`get_index_stats_hook`** | Intercepts index selectivity lookups. | Client Backend | Overrides index selectivity estimates. |
| **`get_attavgwidth_hook`** | Intercepts average attribute width calculations used for memory estimation. | Client Backend | Overrides average attribute width calculations used for memory planning. |
| **`SetPostRewriteHook`** | Attached to a `CachedPlanSource` to inspect/modify query trees after rewriting. | Client Backend | Intercepts rewritten query trees in the plan cache. |

---

## Query Execution & Utility Engine

These hooks wrap the primary statement execution pipeline in PostgreSQL.

| Hook | Invocation & Interception | Process Context | Telemetry Relevance |
| :--- | :--- | :--- | :--- |
| **`ExecutorStart_hook`** | Called at executor initialization; receives `QueryDesc` and execution flags. | Client Backend & Parallel Worker | Initializes execution state, enables per-query buffer usage statistics, and starts top-level statement spans. |
| **`ExecutorRun_hook`** | Called during query plan evaluation; receives `QueryDesc`, scan direction, and row count limit. | Client Backend & Parallel Worker | Measures active tuple iteration and query engine execution time. |
| **`ExecutorFinish_hook`** | Called after tuple iteration finishes, before closing execution state. | Client Backend & Parallel Worker | Detects completion of active plan execution before resource teardown. |
| **`ExecutorEnd_hook`** | Called at the conclusion of query execution; receives `QueryDesc`. | Client Backend & Parallel Worker | Closes execution spans, records processed row counts, calculates buffer reads/hits/writes, and enqueues span telemetry. |
| **`ExecutorCheckPerms_hook`** | Called during executor startup to verify relation and column permissions. | Client Backend | Table and column permission checks; records authorization failures as span events. |
| **`ProcessUtility_hook`** | Wraps non-DML utility commands; receives planned statement, query string, and context. | Client Backend | Instruments DDL, transaction commands, maintenance, and administrative statements. |

---

## Diagnostics, Errors & Logging

PostgreSQL error processing provides two complementary interception mechanisms: producing context into the error stack, and consuming formatted error records.

| Hook / Stack Variable | Invocation & Interception | Process Context | Telemetry Relevance |
| :--- | :--- | :--- | :--- |
| **`error_context_stack`** | Thread-local linked list of callbacks invoked during `ereport()`. | **All Processes** (Client Backend, Workers) | **Context Producer**: Calling `errcontext("...")` injects active OpenTelemetry `trace_id` and `span_id` directly into the error context, making trace identifiers visible in client wire protocol `ErrorResponse` (field `'W'`) and PostgreSQL server logs (`CONTEXT:`). |
| **`emit_log_hook`** | Intercepts fully assembled error/warning records before server output. | **All Processes** (Client Backend, Autovacuum, Checkpointer, Bgworker) | **Telemetry Consumer**: Emits OpenTelemetry log records, attaches exception events to spans, and marks span status as `Error` with the SQLSTATE error code. |

---

## Function Manager (FMGR) Execution

The FMGR hooks allow extensions to intercept every internal C function, built-in SQL function, and procedural language invocation.

| Hook | Invocation & Interception | Process Context | Telemetry Relevance |
| :--- | :--- | :--- | :--- |
| **`needs_fmgr_hook`** | Filter predicate evaluated before calling a function via FMGR; receives the function OID. | Client Backend | Prevents hook overhead on high-frequency primitive operators. |
| **`fmgr_hook`** | Invoked at function entry, normal return, and error abort. | Client Backend | Enables creating child spans for slow stored procedures, UDFs, triggers, and PL/pgSQL routines. |

---

## EXPLAIN Command Hooks

| Hook | Invocation & Interception | Process Context | Telemetry Relevance |
| :--- | :--- | :--- | :--- |
| **`ExplainOneQuery_hook`** | Intercepts top-level `EXPLAIN` query processing; receives the query tree and explain state. | Client Backend | Intercepts top-level `EXPLAIN` query processing. |
| **`explain_per_plan_hook`** | Called for each planned query block within an `EXPLAIN` invocation. | Client Backend | Customizes output per planned query block. |
| **`explain_per_node_hook`** | Called for each node in a plan tree during `EXPLAIN`; receives plan state and ancestor nodes. | Client Backend | Injects custom extension metrics into individual plan nodes during `EXPLAIN (ANALYZE)` output. |
| **`explain_get_index_name_hook`** | Called when formatting index names in explain output; receives index OID. | Client Backend | Customizes index name display in explain outputs. |
| **`explain_validate_options_hook`** | Called to validate custom explain options. | Client Backend | Validates custom options added to the `EXPLAIN (...)` syntax. |

---

## Security, Object Access, and Policies

| Hook | Invocation & Interception | Process Context | Telemetry Relevance |
| :--- | :--- | :--- | :--- |
| **`object_access_hook`** | Intercepts catalog DDL actions; receives class and object OIDs. | Client Backend | Audits creation, alteration, drops, namespace searches, and function executions. Used by audit extensions. |
| **`object_access_hook_str`** | String-based variant for object access events where relation OIDs are not yet allocated. | Client Backend | Auditing object access when identifying objects by name. |
| **`row_security_policy_hook_permissive`** | Called when evaluating row-level security for a command; returns list of permissive policies. | Client Backend | Injects or modifies permissive Row-Level Security (RLS) policies dynamically. |
| **`row_security_policy_hook_restrictive`** | Called when evaluating row-level security for a command; returns list of restrictive policies. | Client Backend | Injects or modifies restrictive Row-Level Security (RLS) policies dynamically. |

---

## Transaction & Teardown Callback Registries

Unlike the global function pointers above, these APIs maintain linked lists of registered callbacks.

### Transaction Callbacks

- **`RegisterXactCallback`**:
  * **Events**: `XACT_EVENT_PRE_COMMIT`, `XACT_EVENT_COMMIT`, `XACT_EVENT_ABORT`, `XACT_EVENT_PREPARE`, `XACT_EVENT_PARALLEL_COMMIT`, etc.
  * **Process Context**: Client Backends, Autovacuum Workers, Background Workers, Parallel Workers.
  * **Telemetry Relevance**: Transaction lifecycle spans, transaction duration calculation via `GetCurrentTransactionStartTimestamp()`, and autovacuum abort/commit detection.
- **`RegisterSubXactCallback`**:
  * **Events**: `SUBXACT_EVENT_START_SUB`, `SUBXACT_EVENT_COMMIT_SUB`, `SUBXACT_EVENT_ABORT_SUB`, `SUBXACT_EVENT_PRE_COMMIT_SUB`.
  * **Process Context**: Client Backends.
  * **Telemetry Relevance**: Savepoint tracking and partial rollback observation.

### Process Exit Callbacks

- **`before_shmem_exit`**: Executed before shared memory structures and locks are invalidated. Critical for flushing active backend IPC buffers to the shared exporter queue prior to process death.
- **`on_shmem_exit`**: Executed during shared memory teardown.
- **`on_proc_exit`**: Executed when the process exits entirely (after shared memory is released). Used for freeing process-private resources.

### Cache & Memory Callbacks

- **Catalog Invalidation (`CacheRegisterSyscacheCallback`, `CacheRegisterRelcacheCallback`)**: Notifies extensions when system catalog tuples or relation cache entries are invalidated by concurrent DDL.
- **Memory Context Reset (`MemoryContextRegisterResetCallback`)**: Invoked when an allocated memory context is reset or deleted. Used for releasing non-memory resources bound to an ephemeral memory context.

### Engine Registration APIs

- **Custom Wait Events (`pgstat_register_custom_wait_event`)** *(PG17+)*: Registers custom extension wait events displayed in `pg_stat_activity` and `pg_stat_wait_event`.
- **Custom WAL Resource Managers (`RegisterCustomRmgr`)** *(PG15+)*: Enables extensions to write, redo, and decode custom WAL records.
- **Background Worker Registration (`RegisterBackgroundWorker`, `RegisterDynamicBackgroundWorker`)**: Registers statically configured background workers in Postmaster or forks dynamic workers on demand from backends.

---

## GUC Configuration

Extensions registering custom GUC parameters can attach callbacks to validate, apply, and display settings:

- **Check Hook**: Validates parameter values upon `SET` or configuration reload. Returns `false` or calls `GUC_check_errdetail()` to reject invalid configurations.
- **Assign Hook**: Invoked when a validated GUC value is committed. Used to apply configuration changes atomically.
- **Show Hook**: Customizes string formatting when querying `SHOW <param>` or `pg_settings`.
