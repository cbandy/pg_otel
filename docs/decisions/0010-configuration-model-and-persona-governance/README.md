---
status: accepted
date: 2026-11-10
deciders: ['@cbandy']
consulted: []
informed: []

---
# ADR 0010: Multi-Dimensional Configuration Model and Persona-Based Governance

## Context and Problem Statement

`pg_otel` originally controlled telemetry export using a single comma-separated string parameter, `otel.export = 'traces, logs'`, defined as `GucContext::Userset`.

As the extension matured, this model revealed two fundamental architectural flaws:

1. **Anti-Tamper & Security Vulnerability**:
   Defining export toggles as `Userset` gives any unprivileged database connection the power to execute `SET otel.export_traces = off` or pass it in their connection string.
   This completely blinds [Security and Compliance Engineers](../../personas.md#security-engineer) and [Platform SREs](../../personas.md#site-reliability-engineer) from detecting unauthorized data access, table exfiltration, or error spikes. In PostgreSQL, access controls are strictly additive; a `USERSET` parameter cannot be revoked or restricted from a user.

2. **Conflation of the Export Pipeline and Instrumentation Sources**:
   `otel.export_traces` bundled two distinct concerns:
   - Operating the cluster-wide OpenTelemetry OTLP network exporter.
   - Instrumenting client SQL statement execution inside backend hooks.
   This prevented granular operational control: disabling `export_traces` to silence noisy statements would silence all traces across the cluster (such as autovacuum worker or checkpoint traces). Conversely, an SRE wanting to observe autovacuum would be forced to accept tracing for millions of OLTP queries per second.

Configuration parameters in `pg_otel` must satisfy distinct, orthogonal dimensions: target scope, operator authority, process binding, and mutation frequency.

PostgreSQL 15 introduced granular parameter access control (`GRANT {SET | ALTER SYSTEM} ON PARAMETER`), allowing administrative capabilities to be delegated to non-superuser roles without superuser sprawl.

How should `pg_otel` categorize its configuration parameters, align them with target personas, enforce non-repudiable audit trails, and provide a scalable foundation for future tracing subsystems (`trace_autovacuum`, `trace_checkpoints`, `trace_indexing`)?

## Decision Drivers

- **Persona Ownership & Least Privilege**: Align configuration parameters strictly with the responsibilities of the personas that operate them ([Target Personas](../../personas.md)).
  Application queries must not dictate cluster egress pipelines; SRE/compliance monitoring must not be tamperable by client connections.
- **Anti-Tamper & Compliance Invariants**: Telemetry export must remain active by default and cannot be suppressed by unprivileged client sessions.
- **Multi-Dimensional Scope**: Accommodate parameters that apply cluster-wide, parameters that vary per database/role/session, and parameters scoped strictly to individual requests.
- **Role Delegation via PostgreSQL 15+ Native Privileges**: Enable DBAs to delegate configuration authority to teams using `GRANT SET ON PARAMETER` and `GRANT ALTER SYSTEM ON PARAMETER` without granting `SUPERUSER`.
- **Multi-Process Architecture Alignment**: Align GUC contexts (`SIGHUP` vs `SUSET` vs `USERSET`) with PostgreSQL's process boundary: singleton background worker (process-isolated, cluster-wide) versus backend hooks (session/process-local).
- **Extensible Subsystem Tracing (`trace_*`)**: Establish a clean separation between the shared OTLP export pipe and individual telemetry generators (`trace_statements`, `trace_autovacuum`, `trace_checkpoints`, `trace_indexing`).
* **Principle of Most Constrained Initial Context**: In PostgreSQL, relaxing a GUC context (e.g. `SIGHUP` → `SUSET` or `SUSET` → `USERSET`) is strictly backward compatible because existing configurations remain valid while new capabilities are unlocked.
  Conversely, tightening a context is a hard breaking change (existing `SET` commands or connection options suddenly throw permission errors).
  Under architectural uncertainty, parameters must start in the most constrained context necessary to protect security and stability, leaving the door open for seamless future relaxation.

## Considered Options

* **Option 1: Monolithic `Userset` Configuration (Status Quo)**
  Keep all switches as `USERSET` for maximum ad-hoc developer convenience.
  *Critique*: Fails compliance and audit non-repudiation. Vulnerable to client-side silencing. Lacks subsystem granularity.

* **Option 2: Strict Superuser Lockdown (`SUSET` across all parameters without delegation)**
  Lock down all settings to superuser-only access.
  *Critique*: Causes superuser credential sprawl. Application developers and query authors are prevented from using `SET LOCAL` for targeted transaction profiling or passing W3C trace context.

* **Option 3: Multi-Dimensional Persona-Driven Configuration Model (Chosen)**
  Structure configuration across operational dimensions (Scope, Authority, Binding, Mutability) spanning 8 functional categories.

## Decision Outcome

Chosen option: **Option 3: Multi-Dimensional Persona-Driven Configuration Model**.

---

## Technical Specifications

### 1. The Four Configuration Dimensions

Every configuration parameter in `pg_otel` is defined by where it sits across four orthogonal axes:

1. **Target Scope**: What entity does the setting configure or describe?
   - `Cluster / Instance`: Global server identity, egress transport, cluster resource caps.
   - `Database`: Tenant-specific observability policies (`ALTER DATABASE`).
   - `Role / User`: Service-account or workload-specific policies (`ALTER ROLE`).
   - `Session`: Temporary connection-level diagnostics (`SET`).
   - `Request / Transaction / Statement`: Dynamic runtime execution context.

2. **Operator Authority & Governance**: Who decides and modifies the setting?
   - `Operator / Administrator`: Platform SRE, DBA, or Security Engineer (`SUSET` / `SIGHUP`).
   - `Delegated Operator`: Non-superuser granted explicit parameter privileges (`GRANT {SET | ALTER SYSTEM} ON PARAMETER`).
   - `Application Developer / Query Author`: Unprivileged client connections and ORMs (`USERSET`).

3. **Architectural Process Binding**: Where in PostgreSQL is the setting evaluated?
   - `Singleton Background Worker`: Process-isolated `exporter_main` managing shared memory queues and HTTP network delivery.
   - `Client Backend Hooks`: Process-local executor, utility, and error hooks (`ExecutorStart`, `ProcessUtility`, `emit_log_hook`).
   - `Active Execution Stack`: Thread-local statement stack and request context (`StatementSpan`).

4. **Mutability & Volatility**: How frequently does the setting change?
   - `Deployment / Boot`: Set at startup, rarely modified.
   - `Reload / SIGHUP`: Changed during infrastructure maintenance without server restarts.
   - `Workload / Connection`: Applied dynamically upon client connection or workload switch.
   - `Per-Request / Per-Query`: Modified continuously per statement or transaction.

---

### 2. The Eight Functional Categories

```
┌──────────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│                                     THE MULTI-DIMENSIONAL CONFIGURATION MODEL                                │
├──────────────────────────────────────────────────────────────────────────────────────────────────────────────┤
│ 1. Identity & Resource     │ • otel.service_name, otel.resource_attributes                                   │
│                            │   Cluster/Instance bound; singleton worker; SIGHUP; reloadable.                 │
├────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────┤
│ 2. OTLP Egress Pipeline    │ • otel.otlp_endpoint, otel.otlp_headers, otel.otlp_timeout, otel.otlp_batch_*   │
│                            │   Cluster/Instance bound; singleton worker; SIGHUP; reloadable.                 │
├────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────┤
│ 3. Extension Activation    │ • (Future) otel.enabled                                                         │
│                            │   Cluster → DB → Role → Session; backend hooks (depth 0 bypass); SUSET.         │
├────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────┤
│ 4. Signal Activation       │ • otel.export_traces, otel.export_logs                                          │
│                            │   Cluster → DB → Role → Session; backend hooks & egress gate; SUSET.            │
├────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────┤
│ 5. Log Selection           │ • (Future) otel.log_min_messages                                                │
│                            │   Cluster → DB → Role → Session; backend emit_log_hook filter; SUSET.           │
├────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────┤
│ 6. Tracing Selection       │ • otel.trace_statements, otel.trace_transactions, otel.trace_parallel_workers   │
│                            │   Cluster → DB → Role → Session (statements/txns/workers); SUSET.               │
│                            │ • (Future) otel.trace_autovacuum, otel.trace_checkpoints                         │
│                            │   Cluster-wide engine workers; SIGHUP; reloadable.                               │
├────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────┤
│ 7. Request Context         │ • otel.sqlcommenter (carrier toggle; SUSET)                                     │
│                            │ • otel.traceparent, otel.tracestate (W3C propagation; USERSET)                   │
├────────────────────────────┼─────────────────────────────────────────────────────────────────────────────────┤
│ 8. Resource Constraints    │ • otel.max_nested_statements (Cluster ceiling; SUSET)                           │
│                            │ • otel.trace_nested_statements (Session request budget; USERSET)                │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

#### Category 1: Service Name & Resource Attributes
- **Parameters**: `otel.service_name`, `otel.resource_attributes`.
- **Scope**: Cluster / Instance.
- **Rationale**: These define the identity of the database entity within the fleet. All telemetry emitted by this PostgreSQL instance shares this resource identity. They do not vary per database, role, or session.
- **Process Binding & Context**: Evaluated exclusively at the singleton background worker (`exporter_main`) when packaging OTLP batches. Defined as `GucContext::Sighup`.

#### Category 2: OTLP Egress Settings
- **Parameters**: `otel.otlp_endpoint`, `otel.otlp_headers`, `otel.otlp_timeout`, `otel.otlp_batch_max_items`, `otel.otlp_batch_max_delay`, `otel.otlp_compression`.
- **Scope**: Cluster / Instance.
- **Rationale**: All telemetry from a single PostgreSQL cluster flows through a single shared-memory queue to a single collector destination. This pipeline does not vary per session or database.
- **Process Binding & Context**: Managed exclusively by the singleton background worker. Defined as `GucContext::Sighup`. Operators manage them via `ALTER SYSTEM SET otel.otlp_* = ...; SELECT pg_reload_conf();`. Delegatable to non-superusers via `GRANT ALTER SYSTEM ON PARAMETER`.

#### Category 3: Extension Activation
- **Parameters**: *(Future)* `otel.enabled`.
- **Scope**: Cluster $\rightarrow$ Database $\rightarrow$ Role $\rightarrow$ Session.
- **Rationale**: While preloaded globally via `shared_preload_libraries`, deployments frequently require granular activation:
  - Multi-tenant clusters may activate `pg_otel` only for specific customer databases (`ALTER DATABASE customer_db SET otel.enabled = on`).
  - High-volume batch ingestion roles or logical replication workers may disable extension overhead completely (`ALTER ROLE etl_user SET otel.enabled = off`).
  - Migration scripts can temporarily disable hooks (`SET otel.enabled = off`).
- **Process Binding & Context**: Evaluated at depth 0 inside backend hooks (`ExecutorStart`, `ProcessUtility`, `emit_log_hook`). If disabled, hooks immediately chain to standard Postgres with zero allocation. Defined as `GucContext::Suset`.

#### Category 4: Signal Activation
- **Parameters**: `otel.export_logs`, `otel.export_traces`.
- **Scope**: Cluster $\rightarrow$ Database $\rightarrow$ Role $\rightarrow$ Session.
- **Rationale**:
  - An e-commerce database may export both traces and logs (`export_traces = on`, `export_logs = on`).
  - An analytics warehouse database may export error logs to OTLP, but disable statement traces to avoid flooding collectors (`export_traces = off`, `export_logs = on`).
  - High-frequency sync accounts can silence traces while keeping logs active (`ALTER ROLE sync_user SET otel.export_traces = off`).
  - Staging tests can toggle export on a single transaction via `SET LOCAL otel.export_traces = on`.
- **Process Binding & Context**: Evaluated inside client backend hooks before allocating spans or pushing to the shared queue. Defined as `GucContext::Suset` so normal users cannot disable them, while allowing `GRANT SET ON PARAMETER` delegation to observability roles.

#### Category 5: Log Selection
- **Parameters**: *(Future)* `otel.log_min_messages`.
- **Scope**: Cluster $\rightarrow$ Database $\rightarrow$ Role $\rightarrow$ Session.
- **Rationale**: Controls the minimum severity of `ErrorData` exported to OTLP (mirroring native `log_min_messages`). A cluster default of `WARNING` can be overridden to `DEBUG` for an ad-hoc troubleshooting session, or clamped to `ERROR` for noisy ETL users.
- **Process Binding & Context**: Evaluated inside `emit_log_hook`. Defined as `GucContext::Suset`.

#### Category 6: Tracing Selection
- **Parameters**: `otel.trace_statements` (default `on`, `SUSET`), `otel.trace_transactions` (default `on`, `SUSET`), `otel.trace_parallel_workers` (default `off`, `SUSET`), *(Future)* `otel.trace_autovacuum` (`SIGHUP`), `otel.trace_checkpoints` (`SIGHUP`), `otel.trace_indexing` (`SIGHUP`).
- **Scope**:
  - Statements, Transactions, Parallel Workers: Cluster $\rightarrow$ Database $\rightarrow$ Role $\rightarrow$ Session (`SUSET`).
  - Core Engine Workers (Autovacuum, Checkpointer): Cluster-wide (`SIGHUP`).
- **Rationale**: Decouples the egress export pipeline from individual telemetry generators:
  - `trace_statements`: Traces individual query and utility statement execution.
  - `trace_transactions`: Traces explicit `BEGIN ... COMMIT / ROLLBACK` boundaries and multi-statement simple-query batches. Allows lightweight transaction duration monitoring without the high cardinality of tracing every statement.
  - `trace_parallel_workers`: Controls whether internal parallel worker processes emit execution sub-spans. Defaults to `off` to prevent unexpected span multipliers (fan-out) on queries that parallelize across multiple workers, while keeping context propagation alive.
  - Engine Workers (`trace_autovacuum`, `trace_checkpoints`): Auxiliary and background worker processes run independently of client backends and do not have SQL sessions or role inheritance. They are cluster-wide operational settings reloaded via `SIGHUP`.
- **Process Binding & Context**:
  - Client Query Execution (`trace_statements`, `trace_transactions`, `trace_parallel_workers`): Evaluated in backend hooks (`ExecutorStart`, `ProcessUtility`, `xact_callback`). Defined as `GucContext::Suset`.
  - Core Engine Workers (`trace_autovacuum`, `trace_checkpoints`): Evaluated in auxiliary/background worker main loops and signal latches. Defined as `GucContext::Sighup` (reloaded via `SELECT pg_reload_conf()`).

#### Category 7: Request Context
- **Parameters**: `otel.sqlcommenter` (default `on`, `SUSET`), `otel.traceparent` (`USERSET`), `otel.tracestate` (`USERSET`).
- **Scope**:
  - Carrier Parser Toggle (`otel.sqlcommenter`): Cluster $\rightarrow$ Database $\rightarrow$ Role $\rightarrow$ Session (`SUSET`).
  - Request Context (`otel.traceparent`, `otel.tracestate`): Request / Transaction / Statement (`USERSET`).
- **Rationale**:
  - `otel.sqlcommenter`: Controls whether the backend scans query strings for SQLCommenter comments (`/* traceparent=... */`). Disabling it avoids string parsing overhead in high-throughput environments and prevents untrusted client SQL from manipulating trace context or sampling flags.
  - `otel.traceparent`, `otel.tracestate`: Carries upstream W3C distributed trace context from application services into the database execution tier.
- **Ambient Context Decoupling**: In accordance with the OpenTelemetry specification (and mirroring the principle behind `OTEL_SDK_DISABLED`), Context Propagation operates independently of span generation. Whenever `export_traces` or `export_logs` is active, incoming context is extracted and held in ambient request storage. This guarantees that **log records emitted during statement execution (`export_logs = on`) correlate with client trace IDs and span IDs even when statement tracing is disabled (`export_traces = off`).**
- **Process Binding & Context**: Ingested in backend hooks and stored in thread-local statement span stacks and ambient context storage. Single-use session GUCs are cleared upon execution. Defined as `GucContext::Userset` for unrestricted client application propagation, while `otel.sqlcommenter` is `SUSET` for operator governance.

#### Category 8: Resourcing Constraints
- **Parameters**: `otel.max_nested_statements`, `otel.trace_nested_statements`.
- **Scope**:
  - Ceiling: Cluster / Instance (`SUSET`).
  - Request Budget: Session / Procedure (`USERSET`).
- **Rationale**: Enforces bounded resource usage (as established in [ADR 0007](../0007-nested-statement-tracing-and-budgeting/README.md)). The DBA enforces a non-negotiable cluster ceiling (`max_nested_statements`), while developers request nested span activation budgets (`trace_nested_statements`) per query.
- **Process Binding & Context**: Evaluated by the backend execution budget manager across procedure call trees.

---

### 3. Dimensional Summary Matrix

| Category | Example Parameters | Scope of Impact | Operator Authority | Evaluated By | Mutability |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **1. Identity & Resource** | `service_name`<br>`resource_attributes` | **Cluster / Instance** | Platform / SRE (`SIGHUP`, `ALTER SYSTEM`) | Singleton Exporter Worker | Reload / SIGHUP |
| **2. OTLP Egress Pipeline** | `otlp_endpoint`<br>`otlp_headers`<br>`otlp_timeout`<br>`otlp_batch_*` | **Cluster / Instance** | Platform / SRE (`SIGHUP`, `ALTER SYSTEM`) | Singleton Exporter Worker | Reload / SIGHUP |
| **3. Extension Activation** | *(Future)* `otel.enabled` | **Cluster $\rightarrow$ DB $\rightarrow$ Role $\rightarrow$ Session** | Platform / DBA (`SUSET`, Grantable) | Backend Hooks (Depth 0 bypass) | Session / Workload |
| **4. Signal Activation** | `export_traces`<br>`export_logs` | **Cluster $\rightarrow$ DB $\rightarrow$ Role $\rightarrow$ Session** | Observability / SRE (`SUSET`, Grantable) | Backend Hooks & Egress Gate | Session / Workload |
| **5. Log Selection** | *(Future)* `log_min_messages` | **Cluster $\rightarrow$ DB $\rightarrow$ Role $\rightarrow$ Session** | Observability / SRE (`SUSET`, Grantable) | Backend `emit_log_hook` | Session / Workload |
| **6. Tracing Selection** | `trace_statements`<br>`trace_transactions`<br>`trace_parallel_workers`<br>*(Future)* `trace_autovacuum`<br>`trace_checkpoints` | **Cluster $\rightarrow$ DB $\rightarrow$ Role $\rightarrow$ Session** (statements/txns/parallel)<br>**Cluster / Instance** (engine workers) | DBA / Developers (`SUSET`)<br>Platform / SRE (`SIGHUP`, `ALTER SYSTEM`) | Backend Query Hooks (`SUSET`)<br>Auxiliary Worker Lifecycles (`SIGHUP`) | Session / Workload (`SUSET`)<br>Reload / SIGHUP (`SIGHUP`) |
| **7. Request Context** | `sqlcommenter` (`SUSET`)<br>`traceparent`<br>`tracestate` (`USERSET`) | **Cluster $\rightarrow$ DB $\rightarrow$ Role $\rightarrow$ Session** (`sqlcommenter`)<br>**Request / Txn / Statement** (`traceparent`, `tracestate`) | Operator (`sqlcommenter`)<br>Developer (`traceparent`) | Ambient Context & Span Stack | Per-query / Per-request |
| **8. Resource Constraints** | `max_nested_statements`<br>`trace_nested_statements` | **Cluster** (ceiling [`SUSET`])<br>**Session** (budget [`USERSET`]) | DBA (ceiling)<br>Developer (budget) | Backend Budget Manager | Session / Procedure Call |

---

### 4. The 2×2 Interaction Matrix: `trace_statements` vs. `export_traces`

| `otel.trace_statements` | `otel.export_traces` | Engine & Telemetry Behavior |
| :--- | :--- | :--- |
| **`on`** | **`on`** | **Full Distributed Tracing**: Statements construct spans, ingest W3C/SQLCommenter context, correlate with parallel query workers, attach to log records, and export OTLP batches over the network. |
| **`off`** | **`on`** | **Export Active, Statements Silenced**: Exporter background worker is active, but backend query hooks return immediately with $O(1)$ zero overhead. Available for background signals (like `trace_autovacuum`) without query span volume. |
| **`on`** | **`off`** | **In-Process Trace Context Only**: Spans and W3C context are tracked in-process, propagated across parallel workers, and attached to log records in `emit_log_hook` (`trace_id` / `span_id`), but spans are dropped rather than enqueued for OTLP export. |
| **`off`** | **`off`** | **Completely Inactive**: Zero instrumentation, zero memory allocation, and zero export overhead. |

---

## Consequences

### Positive Consequences
- **Tamper-Evident Security**: Production applications cannot disable compliance or audit logging. The security floor is enforced by PostgreSQL's C engine definition (`SUSET`).
- **Multi-Tenant & Multi-Workload Flexibility**: `export_*`, `trace_*`, and `enabled` can be tuned per database, per role, or per session without impacting other workloads.
- **Zero Superuser Sprawl**: Leverages native PostgreSQL 15+ role inheritance and `GRANT {SET | ALTER SYSTEM} ON PARAMETER` to delegate exact, fine-grained privileges to observability and development groups.
- **Extensible Subsystem Architecture**: Provides a clean, standardized foundation for upcoming engine instrumentation (`trace_autovacuum`, `trace_checkpoints`, `trace_indexing`).
- **Process Model Fidelity**: Reflects PostgreSQL's actual architecture: cluster-bound settings map to the singleton worker (`SIGHUP`), session-bound settings map to backend hooks (`SUSET` / `USERSET`).
- **Forward Compatibility Guarantee**: Starting parameters in their most constrained viable context (`SIGHUP` for singleton pipeline settings, `SUSET` for signal switches and statement tracing) guarantees that any future context adjustments (relaxing constraints) are strictly backward compatible and will never break existing client connection strings, ORM configurations, or migration scripts.

### Negative Consequences / Trade-offs
- **Developer Friction on Unconfigured Clusters**: Developers cannot toggle `SET otel.export_traces = on` or `SET otel.trace_statements = on` on fresh database clusters without superuser access or an explicit `GRANT SET ON PARAMETER` assigned by the DBA.
- **Parameter Surface Complexity**: Replaces a single monolithic `otel.export` string with orthogonal parameters across categories.

---

## Validation

1. **Permission Enforcement Tests (`pgrx`)**:
   - Verify unprivileged roles running `SET otel.export_traces = off` or `SET otel.trace_statements = off` fail with `ERROR: permission denied to set parameter`.
   - Verify roles granted `GRANT SET ON PARAMETER` can toggle parameters in their session.
2. **Behavioral Matrix Tests**:
   - Verify `trace_statements = off` with `export_traces = on` emits zero statement spans for DML, DDL, and utility commands.
   - Verify `trace_statements = on` with `export_traces = on` emits statement spans as expected.
3. **Cross-Version Compatibility**:
   - Verify compilation and test suite execution across all supported PostgreSQL versions (PG15 through PG19).
