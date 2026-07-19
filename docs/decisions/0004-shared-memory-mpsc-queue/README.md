---
status: accepted
date: 2026-07-19
deciders: ['@cbandy']
consulted: []
informed: []

---
# Shared-Memory MPSC Queue for IPC in pg_otel

## Context and Problem Statement

We need an IPC channel to send telemetry records (logs, metrics, and traces) from client database backends (producers) to a background worker process (consumer).

Historically, the extension used a Unix pipe (`pipe(2)`). However, POSIX pipes have limitations:
1. They are Linux/POSIX-specific, requiring separate implementations or fallback paths for Windows compatibility.
2. Writes larger than `PIPE_BUF` (4KB) are not guaranteed to be atomic, requiring custom chunking and reassembly logic.
3. Pipe reads/writes cross the user/kernel space boundary, incurring system call and context-switching overhead.

How should we design the IPC mechanism to achieve maximum performance, portability, and safety?

## Decision Drivers

* **Performance & Overhead**: Client backends are on the critical path of database queries; overhead must be minimal.
* **Portability**: The implementation should natively support both Linux and Windows.
* **No Blocking**: Under no circumstances should client backends block due to telemetry processing or network delays on the exporter.
* **Process Safety**: Spawning threads in backends is unsafe due to `fork()`.

## Considered Options

### Option A: Unix Pipe (`pipe(2)`) with Custom Chunking
* **Description**: Send messages via a standard Unix pipe, chunking payloads larger than 4KB to maintain POSIX write atomicity.
* **Pros**: Reuses the existing C extension's architecture; simple to implement on Linux.
* **Cons**: No Windows support; kernel space copy/syscall overhead; complex chunk reassembly.

### Option B: Shared Memory MPSC Queue with Drop-Newest Backpressure (Chosen)
* **Description**: Allocate a fixed-size Multi-Producer, Single-Consumer (MPSC) ring buffer in PostgreSQL shared memory. Client backends write to the buffer atomically (lock-free) and signal the background worker via Postgres Latches. If the queue is full, new messages are immediately dropped.
* **Pros**:
  * No system calls or kernel transitions on the hot path (user-space `memcpy` + atomics).
  * Cross-platform (Windows & Linux) out of the box using PostgreSQL's shared memory APIs.
  * No chunking or reassembly required for messages larger than 4KB.
  * Safely prevents backend blocking.
* **Cons**:
  * Requires careful unsafe memory management in Rust.
  * Shared memory corruption (e.g. from bugs in the extension) can PANIC the Postgres cluster.

### Option C: Shared Memory MPSC Queue with Ring Buffer Overwrite (Drop-Oldest)
* **Description**: Similar to Option B, but writes overwrite the oldest unread data when the queue is full.
* **Pros**: Prevents blocking; retains the most recent telemetry.
* **Cons**: High risk of data corruption if a write overwrites the memory segment currently being read by the background worker.

## Decision Outcome

Chosen option: **Option B (Shared Memory MPSC Queue with Drop-Newest Backpressure)**, because it offers high performance (zero-syscall write loop), native cross-platform support across Linux and Windows, and complete safety against backend blocking.

---

## Implementation Guidelines & Rust Architectural Patterns

Future developers implementing or modifying `Queue` in `pg_otel/src/shmem.rs` MUST adhere to the following patterns:

### 1. Header References & Constructor Separation
* **`&'static QueueHeader` for Zero-Unsafe Metadata Accessors:** Store metadata as `header: &'static QueueHeader` in `Queue`. This turns metadata accessors (`get_head`, `set_head`, etc.) into **100% safe Rust code**, eliminating `unsafe` dereferences from getter/setter methods.
* **Constructor Separation (`create` vs `attach`):**
  * `Queue::create()`: Called once by Postmaster during startup. Initializes memory on raw pointers **before** casting to `&'static QueueHeader`. This prevents `invalid_reference_casting` Undefined Behavior.
  * `Queue::attach()`: Called by backends and background worker to attach to an existing running segment without modifying live telemetry state.

### 2. Layout Structs & Type-Safe State Machine
* **`#[repr(C)]` Structs:** Model memory headers (`QueueHeader`) and slot headers (`SlotHeader`) using `#[repr(C)]` structs to eliminate manual byte offset arithmetic.
* **`#[repr(u32)]` Status Enum:** Model slot commitment states using `SlotStatus` (`Free = 0`, `Committed = 1`, `Padding = 2`) rather than integer constants.
* **Encapsulated Memory Orderings:** Hardcode `Acquire` (for reads) and `Release` (for writes) inside methods so callers cannot pass incorrect ordering flags.

### 3. Target Alignment & Bounds Math
* **Target-Aware Alignment (`align_upward`):** Define `ALIGNMENT = core::mem::align_of::<usize>()` and `align_upward(size)` to round slot sizes.
* **Checked Space Calculation:** Use `checked_add` and `.map(align_upward)` on payload sizes in `push()` to guard against integer overflow vulnerabilities leading to heap/buffer overflows.
* **Saturating Boundary Checks:** Use `saturating_add()` for wrap-around checks to prevent overflow panics.
* **Wrapping Monotonic Math (Subtract Before Add):** Use `wrapping_add` and `wrapping_sub` on `tail` and `head` counters.

### 4. Thread-Safety Gating
* **`#[cfg(test)]` Gating for `Send` / `Sync`:** In production multi-process execution, `Queue` remains strictly `!Send` and `!Sync` to provide compiler guard rails against thread misuse, while enabling standard multi-threaded Rust unit tests.

---

## Validation

The implementation is validated via:
1. Unit tests in `pg_otel/src/shmem.rs`.
2. `cargo test -p pg_otel --lib` verifying cross-module build safety and zero compiler warnings/errors.
