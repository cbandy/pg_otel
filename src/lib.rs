// SPDX-License-Identifier: ISC

// Add the following to "${PGRX_HOME}/data-${PGVER}/postgresql.conf" for `cargo pgrx run`:
//
//     shared_preload_libraries = 'pg_otel'

// https://doc.rust-lang.org/stable/edition-guide/rust-2024/static-mut-references.html
#![deny(static_mut_refs)]
// https://doc.rust-lang.org/stable/edition-guide/rust-2024/unsafe-op-in-unsafe-fn.html
#![deny(unsafe_op_in_unsafe_fn)]

mod config;
mod ipc;

use pgrx::pg_sys;

const PG_OTEL_LIBRARY: &str = env!("CARGO_PKG_NAME");
#[cfg(any())]
const PG_OTEL_VERSION: &str = env!("CARGO_PKG_VERSION");

pgrx::pg_module_magic!(name, version);

pub(crate) fn assert_postmaster_startup() -> bool {
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
    // Panic if the extension is loaded after Postgres startup.
    assert_postmaster_startup();

    // Define our GUC variables.
    crate::config::define_guc_variables();
}

// This module must be visible at the root of the crate to configure `cargo pgrx test`.
#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}

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
