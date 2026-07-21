// SPDX-License-Identifier: MIT

mod exporter;
mod logging;
mod otlp;
mod shmem;

pub(crate) use prost::bytes::BytesMut;

use pgrx::pg_sys;

const PG_OTEL_LIBRARY: &str = env!("CARGO_PKG_NAME");
const PG_OTEL_VERSION: &str = env!("CARGO_PKG_VERSION");
pgrx::pg_module_magic!(name, version);

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

#[inline(always)]
const fn unlikely<T>(v: T) -> T {
    core::hint::cold_path();
    v
}

/// Postmaster calls this once, just after the module is dynamically loaded.
/// It panics when not loaded via the "shared_preload_libraries" parameter.
#[allow(non_snake_case)]
#[pgrx::pg_guard]
pub extern "C-unwind" fn _PG_init() {
    // Panic if the extension is loaded after Postgres startup.
    assert!(assert_postmaster_startup());

    crate::exporter::install_hooks();
    crate::logging::install_hooks();
}

// This module must be visible at the root of the crate for `#[pg_test]` functions.
#[cfg(test)]
pub mod pg_test {
    /// Each `#[pg_test]` function calls this from the Rust test binary before initializing Postgres.
    pub fn setup(_attributes: Vec<&str>) {}

    /// Each `#[pg_test]` function calls this while initializing Postgres.
    #[must_use]
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
