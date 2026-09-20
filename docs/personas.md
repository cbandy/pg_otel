# Target Personas

This document describes the key personas that `pg_otel` is designed to serve, including their primary goals, operational constraints, and the design impact on the extension.

---

## 1. PostgreSQL DBA / Platform Engineer

- **Profile**: Responsible for database availability, performance, stability, and fleet-wide configuration (`postgresql.conf`, `shared_preload_libraries`).
- **Goals**:
  * Ensure telemetry generation never destabilizes the database or degrades transactional query throughput.
  * Safely deploy the extension across database fleets with minimal operational friction.
- **Key Constraints & Pain Points**:
  * Strong aversion to extensions that can crash backends, leak memory, or hold long-lived locks.
  * Vigilant against **Transaction ID (XID) wraparound** and bloat caused by long-running transactions or pinned snapshots.
  * Manages checkpoint write storms, WAL saturation, and disk-spilling queries that exhaust disk IOPS and storage bandwidth.
  * Demands predictable resource caps (shared memory buffer size, background worker CPU/memory limits).
  * Requires dynamic reconfigurability without database restarts (`pg_reload_conf()`, GUCs).
- **Design Impact on `pg_otel`**:
  * Safe-by-default: loading the library does nothing until explicitly enabled via `otel.export`.
  * Invariant: backends and hooks must never block client queries or spawn threads. Backpressure must drop telemetry rather than pause transactions.
  * Configuration exposed as standard PostgreSQL GUCs (`otel.*`).

---

## 2. Observability / SRE Engineer

- **Profile**: Operates monitoring pipelines, OpenTelemetry collectors, and downstream telemetry backends.
- **Goals**:
  * Ingest high-fidelity database telemetry alongside infrastructure and application data.
  * Standardize telemetry schemas to avoid custom parsing or vendor lock-in.
- **Key Constraints & Pain Points**:
  * Needs database telemetry to correlate directly with host/storage metrics (e.g., matching query latency spikes to disk IOPS and queue depth).
  * Frustrated by proprietary database log formats that require complex regex parsing or sidecar log-tailing agents.
  * Needs telemetry payloads that conform strictly to OpenTelemetry semantic conventions.
  * Sensitive to exporter network backpressure and collector downtime.
- **Design Impact on `pg_otel`**:
  * Direct OTLP export (HTTP/Protobuf) from a dedicated background worker to standard OpenTelemetry collectors.
  * Adherence to OpenTelemetry Semantic Conventions for database systems (`db.system`, `db.statement`, etc.).
  * Standard OpenTelemetry environment variables (`OTEL_EXPORTER_OTLP_ENDPOINT`, etc.) respected alongside PostgreSQL GUCs.

---

## 3. Application Developer / Query Author

- **Profile**: Builds client services that query PostgreSQL (via ORMs or raw SQL drivers) and diagnoses end-to-end latency in distributed architectures.
- **Goals**:
  * Correlate application-tier traces with specific database queries to pinpoint slow transactions and N+1 query patterns.
  * Understand internal query execution timing without needing privileged DBA access to PostgreSQL log files.
- **Key Constraints & Pain Points**:
  * Loss of distributed context when requests enter the database tier (database appears as a "black box" span).
  * Hard-to-diagnose application bugs that leave connections `idle in transaction`.
  * Cannot modify database connection settings per query; often routes through connection poolers.
- **Design Impact on `pg_otel`**:
  * Transparent W3C `traceparent` and `tracestate` context propagation via SQL comments (SQLCommenter) or session-level GUCs.
  * Statement-level span generation capturing execution duration, command tags, and database error states.
  * **Transaction Lifecycle Spans**: Visibility into transaction boundaries and durations spent `idle in transaction` to identify leaked connections.

---

## 4. Database Developer / Procedural SQL Developer

- **Profile**: Designs and maintains in-database business logic, including stored procedures, user-defined functions, triggers, and complex views.
- **Goals**:
  * Profile and optimize procedural code, loops, nested function calls, and cascading triggers.
  * Diagnose internal query latencies, lock contention, and error states without relying solely on manual `RAISE NOTICE` logging.
- **Key Constraints & Pain Points**:
  * External APM tools treat a procedure or function call as a single opaque span, hiding which internal statements or loop iterations consumed execution time.
  * PL/pgSQL `BEGIN ... EXCEPTION WHEN ...` blocks can catch and mask errors, making failure paths difficult to observe from client telemetry.
  * An apparently simple `INSERT` or `UPDATE` stalls because an unindexed or cascading trigger executes hidden queries via SPI.
- **Design Impact on `pg_otel`**:
  * Tracing must track nested execution and distinguish top-level user queries from inner SPI calls and trigger executions.
  * Leveraging `emit_log_hook` to capture `RAISE WARNING` / `RAISE NOTICE` outputs and associate them directly with the executing span.

---

## 5. Security & Compliance Engineer

- **Profile**: Ensures database access adheres to regulatory frameworks and monitors for unauthorized data access, privilege abuse, and breaches.
- **Goals**:
  * Maintain an auditable, tamper-evident log of sensitive data access and administrative operations.
  * Rapidly correlate database events with authenticated user identities from the application tier.
- **Key Constraints & Pain Points**:
  * Generic connection pools obscure the real end-user identity in traditional audit logs.
  * Traditional audit solutions output massive, semistructured logs that are expensive to ingest and parse in SIEM platforms.
- **Design Impact on `pg_otel`**:
  * Propagate authenticated end-user identifiers (e.g., via W3C baggage, traceparent, or SQLCommenter) into span and log attributes.
  * Emit structured security events for authentication failures and authorization errors.

---

## 6. Data / Analytics Engineer

- **Profile**: Builds and operates data pipelines, orchestrates batch transformations, and runs analytical queries.
- **Goals**:
  * Optimize long-running batch transformations, bulk loads, and analytical aggregation queries.
  * Isolate lock contention between analytical batch jobs and transactional OLTP workloads.
- **Key Constraints & Pain Points**:
  * Analytical queries frequently spill large datasets to disk (temporary files), bottlenecking I/O without clear visibility from standard APMs.
  * Matching database workload back to specific DAGs, jobs, or models is difficult without automated query tagging.
- **Design Impact on `pg_otel`**:
  * Capture orchestrator metadata from SQLCommenter comments directly as span attributes.
  * Record temporary file disk spills and row counts on statement spans.

---

# Shared Operational Scenarios

Real-world database incidents often intersect these personas. `pg_otel` bridges the communication and diagnostic gap across application code, procedural database logic, analytics pipelines, and engine internals:

| Operational Scenario | DBA Experience | App & Database Developer Experience | How `pg_otel` Bridges the Gap |
| :--- | :--- | :--- | :--- |
| **"Idle in Transaction" Connections** | Bloat, lock starvation, connection pool exhaustion, and stalled vacuuming. | App dev unaware until pool fails; DB dev investigates long-lived procedural transaction blocks. | Trace context links the transaction directly to the upstream client endpoint and service that initiated it. |
| **Hidden Trigger / Procedure Latency** | Unexplained CPU/memory consumption on what appears to be a lightweight query. | App dev blames the database for slow `INSERT`s; DB dev struggles to isolate which trigger/SPI call took time. | Detailed span attributes attribute latency to specific procedures and triggers. |
| **Disk Performance Degradation** | High IOPS, checkpoint spikes, WAL generation surges, and disk fill alerts. | Query latency degradation or statement timeouts. | Correlates query spans with disk-spill metrics (temp files from analytical/ETL queries) and buffer cache misses. |
| **Transaction ID (XID) Wraparound** | Urgent maintenance to avoid forced read-only shutdown; struggles to find which session is holding back the freeze horizon. | Unaware that their long-running transactions or write loops are exhausting the global 32-bit counter or pinning `xmin`. | Trace context links the errant transaction to the exact upstream service/endpoint; real-time metrics track XID burn rate and headroom; `emit_log_hook` routes early wraparound warnings to alerts. |
| **Audit & Security Investigation** | SecOps/DBA alerted to suspicious queries or permission failures on sensitive tables. | Security team cannot identify which human user made the request through the generic pooled connection. | Distributed trace context and SQLCommenter attributes map the query directly back to the authenticated user ID and API request. |
