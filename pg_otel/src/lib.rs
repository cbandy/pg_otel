// SPDX-License-Identifier: MIT

mod exporter;
mod logging;
mod otlp;
mod shmem;
mod tracing;

pub(crate) use prost::bytes::BytesMut;

use pgrx::pg_sys;
use std::cell::Cell;
use std::ffi;

thread_local! { static ENABLED: Cell<Signals> = Cell::new(Signals::default()); }

const PG_OTEL_LIBRARY: &str = env!("CARGO_PKG_NAME");
const PG_OTEL_VERSION: &str = env!("CARGO_PKG_VERSION");
pgrx::pg_module_magic!(name, version);

type GucInt32 = pgrx::guc::GucSetting<i32>;
type GucString = pgrx::guc::GucSetting<Option<ffi::CString>>;

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

    crate::define_guc_variables();
    crate::exporter::define_guc_variables();
    crate::exporter::install_hooks();
    crate::logging::install_hooks();
    crate::tracing::install_hooks();
}

fn define_guc_variables() {
    use pgrx::guc::{GucCheckError, GucContext, GucFlags, GucRegistry};

    static GUC_SIGNALS_ENABLED: GucString = GucString::new(None);
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"pg_otel.export",
            c"Telemetry signals to export",
            c"Comma-separated list of signals to export (traces, logs).",
            &GUC_SIGNALS_ENABLED,
            GucContext::Userset,
            GucFlags::empty()
                | GucFlags::from_bits_retain(pg_sys::GUC_IS_NAME as i32)
                | GucFlags::from_bits_retain(pg_sys::GUC_LIST_INPUT as i32)
                | GucFlags::from_bits_retain(pg_sys::GUC_NOT_WHILE_SEC_REST as i32),
            Some(check),
            Some(assign),
            None,
        );

        #[pgrx::pg_guc_hook(check)]
        fn check(value: Option<ffi::CString>) -> Result<(), GucCheckError> {
            if let Some(value) = value.as_deref() {
                value
                    .to_str()
                    .map_err(eyre::Report::from)
                    .and_then(Signals::parse)
                    .map_err(|e| GucCheckError::new(e.to_string()))?;
            }
            Ok(())
        }

        #[pgrx::pg_guc_hook(assign)]
        fn assign(value: Option<ffi::CString>) {
            let parsed = value
                .map(|v| Signals::parse(&v.to_string_lossy()).unwrap())
                .unwrap_or_default();

            ENABLED.set(parsed);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Signals(u8);

impl Signals {
    pub fn logs(&self) -> bool {
        self.0 & 0b01 != 0
    }
    pub fn spans(&self) -> bool {
        self.0 & 0b10 != 0
    }

    fn parse(raw: &str) -> eyre::Result<Self> {
        let mut result = Self::default();
        for part in raw.split(',') {
            match part.trim() {
                "" => (),
                "log" | "logs" => result.0 |= 0b01,
                "span" | "spans" | "trace" | "traces" => result.0 |= 0b10,
                v => eyre::bail!("unrecognized signal: {:?}", v),
            }
        }
        Ok(result)
    }
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
    /// Each `#[pg_test]` function calls this from the Rust test binary before connecting to Postgres.
    /// Comma-separated arguments to the macro arrive in the `Vec` here, e.g. `#[pg_test(A, B, C)]`.
    pub fn setup(_attributes: Vec<&str>) {}

    /// Each `#[pg_test]` function calls this from the Rust test binary after disconnecting.
    pub fn teardown() {}

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

#[cfg(any(test, feature = "pg_test"))]
pub fn connect_test_client() -> postgres::Client {
    let conn_str: String = pgrx::Spi::get_one(
        "SELECT format('host=%L port=%L user=%L dbname=%L', \
                split_part(current_setting('unix_socket_directories'), ',', 1), \
                current_setting('port'), current_user, current_database())",
    )
    .unwrap()
    .unwrap();

    postgres::Client::connect(&conn_str, postgres::NoTls).unwrap()
}

#[cfg(test)]
mod tests {
    use super::Signals;
    use googletest::prelude::*;

    #[test]
    fn signals() {
        assert_that!(Signals::default().logs(), eq(false));
        assert_that!(Signals::default().spans(), eq(false));
        assert_that!(Signals::parse(""), ok(eq(&Signals::default())));
        assert_that!(Signals::parse(",, "), ok(eq(&Signals::default())));

        assert_that!(Signals(1).logs(), eq(true));
        assert_that!(Signals(1).spans(), eq(false));
        assert_that!(Signals::parse("log"), ok(eq(&Signals(1))));
        assert_that!(Signals::parse("logs"), ok(eq(&Signals(1))));

        assert_that!(Signals(2).logs(), eq(false));
        assert_that!(Signals(2).spans(), eq(true));
        assert_that!(Signals::parse("span"), ok(eq(&Signals(2))));
        assert_that!(Signals::parse("traces"), ok(eq(&Signals(2))));

        assert_that!(Signals(3).logs(), eq(true));
        assert_that!(Signals(3).spans(), eq(true));
        assert_that!(Signals::parse("trace,log"), ok(eq(&Signals(3))));
        assert_that!(Signals::parse("logs, span"), ok(eq(&Signals(3))));

        assert_that!(Signals::parse("all"), err(anything()));
        assert_that!(Signals::parse("none"), err(anything()));
        assert_that!(Signals::parse("logs,none"), err(anything()));
        assert_that!(Signals::parse("off"), err(anything()));
        assert_that!(Signals::parse("invalid"), err(anything()));
    }
}
