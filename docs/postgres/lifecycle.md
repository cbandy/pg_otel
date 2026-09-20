# PostgreSQL Architecture and Process Lifecycles

This document serves as a technical reference for PostgreSQL's multi-process supervisor model, process execution lifecycles, extension shared-memory initialization, signal handling, and shutdown procedures.

---

## Architecture Overview

PostgreSQL uses a **multi-process model** centered around a main supervisor process called **Postmaster**.

```mermaid
flowchart TD
    PM["Postmaster (Main Supervisor Process)"]
    Shmem["OS Shared Memory Segment (IPC Buffer & LWLocks)"]

    PM -->|Allocates & Supervises| Shmem
    PM -->|forks at startup| BG["Background Worker Processes (e.g. Exporter, Autovacuum)"]
    PM -->|forks per TCP connection| BE["Client Backend Processes (Query Execution)"]

    BG <-->|IPC via Atomics & Latches| Shmem
    BE <-->|IPC via Atomics & Latches| Shmem
```

---

## 1. Postmaster Startup and Shared Memory

Postmaster initializes state, parses configuration files, allocates shared memory, and loads C extensions specified in `shared_preload_libraries`.

### Startup Sequence Diagram

```mermaid
sequenceDiagram
    autonumber
    participant PM as Postmaster (Supervisor)
    participant Ext as Extension DLL/SO (_PG_init)
    participant GUC as GUC Config Engine
    participant Shmem as OS Shared Memory

    PM->>GUC: 1. InitializeGUCOptions() [Set Built-in Defaults]
    PM->>Ext: 2. Load shared_preload_libraries & Call _PG_init()
    Note over Ext: Extensions register custom GUCs & background workers here
    PM->>GUC: 3. Read postgresql.conf [Populate Configured GUC Values]

    opt PostgreSQL 15+
        PM->>Ext: 4. shmem_request_hook()
        Note over Ext: Extensions request shmem bytes & LWLocks (RequestAddinShmemSpace)
    end

    PM->>Shmem: 5. Allocate OS Shared Memory Segment (mmap/shmget)

    PM->>Ext: 6. shmem_startup_hook()
    Note over Ext: Extensions bind pointers to shmem sub-segments (ShmemInitStruct)

    PM->>PM: 7. Fork Background Workers & Accept TCP Client Connections
```

### General Extension Hook Points at Startup

* **`_PG_init()`**: Called by Postmaster immediately when the extension's dynamic library is loaded. Extensions use this entrypoint to define custom GUC variables (`DefineCustomIntVariable`), register background workers (`RegisterBackgroundWorker`), and set up hook function pointers.
* **`shmem_request_hook` _(PG15+)_**: Executed during Postmaster shared memory size estimation, **before** OS memory allocation. Extensions call `RequestAddinShmemSpace(bytes)` and `RequestNamedLWLockTranche()` here. *(In PG13/14, `RequestAddinShmemSpace` was called in `_PG_init`)*.
* **`shmem_startup_hook`**: Executed after OS shared memory is allocated. Extensions call `ShmemInitStruct("name", size, &mut found)` to initialize or attach to their shared memory structures.

---

## 2. Client Connection and Authentication

When a client connects, Postmaster immediately forks a dedicated **Backend Process**.

```mermaid
sequenceDiagram
    autonumber
    participant Client as DB Client
    participant PM as Postmaster (Listening Socket)
    participant BE as Backend Process (Forked)

    Note over PM: Postmaster listens on TCP port (or Unix Socket)
    Client->>PM: 1. TCP Connection Established (SYN/ACK)
    PM->>BE: 2. Immediate fork()
    Note over PM: Postmaster closes its copy of socket & resumes listening.
    Note over BE: Child Backend inherits socket & attaches to Shared Memory.

    BE->>Client: 3. SSL/TLS Negotiation (if enabled)
    Client->>BE: 4. StartupMessage (Database, User, Protocol Options)
    BE->>BE: Match connection against pg_hba.conf
    BE->>Client: 5. AuthenticationRequest (SCRAM-SHA-256, MD5, Cert, Trust)
    Client->>BE: 6. Password Proof / Auth Token
    BE->>BE: Verify credentials

    alt Authentication Fails
        BE->>Client: ErrorResponse (FATAL: authentication failed)
        BE->>BE: Child backend process exits immediately
    else Authentication Succeeds
        BE->>Client: AuthenticationOk + ParameterStatus + ReadyForQuery
        Note over BE: Enter main query processing loop (PostgresMain)
    end
```

---

## 3. Signal Handling, Configuration Reloads, and Shutdown

PostgreSQL processes communicate shutdown requests and configuration reloads via UNIX signals and Latch primitives.

### Latch Synchronization (`WaitLatch` / `SetLatch`)

In PostgreSQL's process architecture, background workers sleep using `WaitLatch()`. When another process (a backend or Postmaster) needs to wake up the worker, it calls `SetLatch(&worker->latch)`. This provides a zero-overhead, event-driven sleeping and waking mechanism without polling.

### Shutdown & Signal Handling Diagram

```mermaid
sequenceDiagram
    autonumber
    participant Admin as Administrator / Systemd
    participant PM as Postmaster
    participant BG as Background Worker
    participant BE as Client Backends

    alt SIGHUP (Config Reload)
        Admin->>PM: SIGHUP / pg_reload_conf()
        PM->>BG: Forward SIGHUP
        PM->>BE: Forward SIGHUP
        Note over BG,BE: Processes call ProcessConfigFile(PGC_SIGHUP) to update GUCs
    else Shutdown Modes (Smart / Fast / Immediate)
        Admin->>PM: SIGTERM (Fast) / SIGINT (Smart)
        PM->>BE: Send SIGTERM (Close sessions)
        PM->>BG: Send SIGTERM to Background Workers
        Note over BG: Background Worker wait_latch() returns false & process exits 0
        PM->>PM: Wait for processes to exit & Free OS Shared Memory
    end
```

### PostgreSQL Shutdown Modes

| Shutdown Mode | Signal | Behavior |
| :--- | :--- | :--- |
| **Smart Shutdown** | `SIGINT` | Disallows new connections; waits for existing client sessions and background workers to finish. |
| **Fast Shutdown** _(Default)_ | `SIGTERM` | Terminates active client sessions, rolls back active transactions, signals background workers, and shuts down immediately. |
| **Immediate Shutdown** | `SIGQUIT` | Postmaster terminates all child processes immediately without clean shutdown, requiring crash recovery on next startup. |

### PostgreSQL Shutdown Phase Ordering

During server shutdown (`Fast` or `Smart`), PostgreSQL manages process termination in distinct phases to ensure data integrity:

1. **Phase 1**: `PM_WAIT_BACKENDS`
   * Postmaster sends `SIGTERM` to all **client backends**, **autovacuum launcher/workers**, **parallel query workers**, and custom **background workers**.
   * Postmaster waits for all Phase 1 processes to exit.
2. **Phase 2**: `PM_SHUTDOWN`
   * Postmaster intentionally keeps auxiliary processes (**checkpointer**, **background writer**, **WAL writer**) running throughout Phase 1 so those backends and workers can finish logging WAL and flushing pages.
   * Postmaster signals auxiliary processes to execute the final shutdown checkpoint and exit **only after all Phase 1 processes have exited**.
