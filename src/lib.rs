// SPDX-License-Identifier: ISC

// Add the following to "${PGRX_HOME}/data-${PGVER}/postgresql.conf" for `cargo pgrx run`:
//
//     shared_preload_libraries = 'pg_otel'

// https://doc.rust-lang.org/stable/edition-guide/rust-2024/static-mut-references.html
#![deny(static_mut_refs)]
// https://doc.rust-lang.org/stable/edition-guide/rust-2024/unsafe-op-in-unsafe-fn.html
#![deny(unsafe_op_in_unsafe_fn)]

mod config;
mod export;
mod ipc;
mod logging;

use pgrx::pg_sys;
use std::sync::{Mutex, OnceLock};

const PG_OTEL_LIBRARY: &str = env!("CARGO_PKG_NAME");
#[cfg(any())]
const PG_OTEL_VERSION: &str = env!("CARGO_PKG_VERSION");

pgrx::pg_module_magic!(name, version);

// These variables are initialized during [`_PG_init()`], inherited by Postgres backends, and
// inherited by this extension's background workers.
static AT_EXPORTER: OnceLock<Mutex<Option<crate::ipc::Reader>>> = OnceLock::new();
static TO_EXPORTER: OnceLock<Mutex<crate::ipc::Writer>> = OnceLock::new();

#[must_use]
fn assert_postmaster_startup() -> bool {
    // This panics when called from a thread other than the main one.
    pg_sys::thread_check::check_active_thread();

    // SAFETY: It is safe to read this variable from the main thread.
    if unsafe { !pg_sys::process_shared_preload_libraries_in_progress } {
        pgrx::ereport!(
            pgrx::PgLogLevel::ERROR,
            pgrx::PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("{PG_OTEL_LIBRARY} must be loaded via shared_preload_libraries"),
        )
    }

    // Return a value so this call can be passed to debug_assert!.
    true
}

/// Called when the module is loaded.
#[allow(non_snake_case)]
#[pgrx::pg_guard]
pub extern "C-unwind" fn _PG_init() {
    use pgrx::bgworkers::{BackgroundWorkerBuilder, BgWorkerStartTime};
    use std::time::Duration;

    // Panic if the extension is loaded after Postgres startup.
    assert!(assert_postmaster_startup());

    // Open communication to the exporter background worker.
    // These owned values are inherited by the forked background worker and backend servers.
    //
    // NOTE: After fork(2), either process may close their *copy* without affecting others.
    let (r, w) = crate::ipc::new().unwrap();
    AT_EXPORTER.set(Mutex::new(Some(r))).unwrap();
    TO_EXPORTER.set(Mutex::new(w)).unwrap();

    // Define our GUC variables.
    crate::config::define_guc_variables();

    // Set the "extra" value so hooks know when they are running inside this worker.
    BackgroundWorkerBuilder::new("OpenTelemetry exporter")
        .set_start_time(BgWorkerStartTime::PostmasterStart)
        .set_restart_time(Some(Duration::from_secs(1)))
        .set_function("exporter_worker_main")
        .set_library(PG_OTEL_LIBRARY)
        .set_extra("E")
        .enable_shmem_access(None) // https://github.com/pgcentralfoundation/pgrx/issues/2160
        .load();

    crate::logging::install_hooks();
}

/// The entrypoint for the exporter background worker.
/// Postmaster calls this after fork(2) and some housekeeping.
#[pgrx::pg_guard]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn exporter_worker_main(_arg: pg_sys::Datum) {
    use pgrx::bgworkers::{BackgroundWorker, SignalWakeFlags};

    // Immediately register handlers and unblock signals.
    // These handlers set MyLatch, ConfigReloadPending, and ShutdownRequestPending.
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);

    pgrx::log!("{} is starting", BackgroundWorker::get_name());

    let reader = AT_EXPORTER.wait().lock().unwrap().take().unwrap();
    let mut events = crate::ipc::WaitEventSet::new(3);
    events.expect_latch_set(unsafe { pg_sys::MyLatch });
    events.expect_postmaster_death();
    events.expect_readable(&reader);

    let mut pipeline = crate::export::Pipeline::new(unsafe { crate::config::loaded() });
    let mut receiver: crate::ipc::SyncReceiver = reader.into();

    loop {
        let event = events.wait_forever();

        // Reset the latch set by signal handlers first.
        if (event.events & pg_sys::WL_LATCH_SET) != 0 {
            unsafe { pg_sys::ResetLatch(pg_sys::MyLatch) };
        }

        // Begin shutdown when Postmaster dies or sends SIGTERM.
        //
        // The SIGTERM handler installed by pgrx sets ShutdownRequestPending.
        if (event.events & pg_sys::WL_POSTMASTER_DEATH) != 0 {
            break;
        }
        if BackgroundWorker::sigterm_received() || unsafe { pg_sys::ShutdownRequestPending } != 0 {
            break;
        }

        // Read and apply changes to GUC values when Postmaster sends a SIGHUP.
        //
        // The SIGHUP handler installed by pgrx sets ConfigReloadPending.
        if BackgroundWorker::sighup_received() || unsafe { pg_sys::ConfigReloadPending } != 0 {
            unsafe {
                pg_sys::ConfigReloadPending = 0;
                pg_sys::ProcessConfigFile(pg_sys::GucContext::PGC_SIGHUP);
            }

            // Stand up a new set of exporters with the new configuration.
            // This closes the prior channel, and its exporters drain it and exit.
            pipeline = crate::export::Pipeline::new(unsafe { crate::config::loaded() });
        }

        // Read from IPC and dispatch any messages there.
        if (event.events & pg_sys::WL_SOCKET_READABLE) != 0 {
            pipeline.ingest(receiver.recv());

            while receiver.is_buffered() {
                pipeline.ingest(receiver.recv());
            }
        }
    }

    // Dispatch any messages remaining in the IPC buffer before handing off to Postmaster.
    while receiver.is_buffered() {
        pipeline.ingest(receiver.recv());
    }

    // TODO: wait for pipeline to drain.

    pgrx::log!("{} stopped", BackgroundWorker::get_name());
}

// This module must be visible at the root of the crate for `#[pg_test]` functions.
#[cfg(test)]
pub mod pg_test {
    /// Each `#[pg_test]` function calls this from the Rust test binary before initializing Postgres.
    pub fn setup(_attributes: Vec<&str>) {}

    /// Each `#[pg_test]` function calls this while initializing Postgres.
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        assert_eq!(super::PG_OTEL_LIBRARY, "pg_otel");
        vec!["shared_preload_libraries = 'pg_otel'"]
    }

    // Every function annotated with `#[pg_test]` MUST be inside a module named "tests" with
    // these attributes:
    //
    // ```rust
    // #[cfg(any(test, feature = "pg_test"))]
    // #[pgrx::pg_schema]
    // mod tests {}
    // ```
    //
    // https://github.com/pgcentralfoundation/pgrx/issues/1259
    // https://github.com/pgcentralfoundation/pgrx/issues/1612
}
