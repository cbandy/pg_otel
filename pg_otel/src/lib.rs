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

    crate::exporter::define_guc_variables();
    crate::exporter::install_hooks();
    crate::logging::install_hooks();
}

/// This module must be visible at the root of the crate for `#[pg_test]` functions. The Rust test
/// binary uses it to initialize Postgres.
///
/// The Rust test binary is built with `#[cfg(all(test, feature = "pg_test"))]`.
/// The library loaded into Postgres by `pgrx test` is built with `#[cfg(feature = "pg_test")]`.
/// The library built by `pgrx package` has neither.
///
/// Every function annotated with `#[pg_test]` MUST be inside a module named "tests" with these
/// attributes:
///
/// ```rust
/// #[cfg(any(test, feature = "pg_test"))]
/// #[pgrx::pg_schema]
/// mod tests {}
/// ```
///
/// https://github.com/pgcentralfoundation/pgrx/issues/1259
/// https://github.com/pgcentralfoundation/pgrx/issues/1612
#[cfg(test)]
pub mod pg_test {
    /// Each `#[pg_test]` function calls this from the Rust test binary before initializing Postgres.
    /// Comma-separated arguments to the macro arrive in the `Vec` here, e.g. `#[pg_test(A, B, C)]`.
    pub fn setup(_attributes: Vec<&str>) {}

    /// The first `#[pg_test]` function to run calls this (once) from the Rust test binary.
    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        let addr = crate::exporter::tests::HTTP_OTLP_SERVER.clone();
        let endpoint = format!("pg_otel.endpoint = 'http://{}'", addr);
        let preload = format!("shared_preload_libraries = '{}'", crate::PG_OTEL_LIBRARY);

        vec![
            "pg_otel.batch_max_delay = '10ms'",
            "pg_otel.timeout = '5s'",
            Box::leak(endpoint.into_boxed_str()),
            Box::leak(preload.into_boxed_str()),
        ]
    }
}

#[cfg(any(test, feature = "pg_test"))]
pub fn acquire_test_lock() {
    const FNV: u64 = 0x922B03880484F257; // FNV-1a of "pg_otel"
    const KEY: i64 = FNV as i64;
    pgrx::Spi::get_one::<()>(&format!("SELECT pg_advisory_xact_lock({KEY})"))
        .expect("failed to acquire pg_advisory_xact_lock");
}
