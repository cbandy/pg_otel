// SPDX-License-Identifier: ISC

pub(crate) use crate::config_types::ExportSignal::*;
use crate::config_types::*;
use opentelemetry_otlp as otlp;
use pgrx::GucSetting;
use pgrx::prelude::*;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

type ParameterInt = GucSetting<i32>;
type ParameterStr = GucSetting<Option<CString>>;

// These static variables are populated by PostgreSQL during ProcessConfigFile.
// - https://doxygen.postgresql.org/guc_8h.html

static OTEL_ATTRIBUTE_COUNT_LIMIT: ParameterInt = ParameterInt::new(128);
static OTEL_EXPORTS: ParameterStr = ParameterStr::new(None);
static OTEL_SERVICE_NAME: ParameterStr = ParameterStr::new(Some(c"postgresql"));

static OTEL_OTLP_COMPRESSION: ParameterStr = ParameterStr::new(None);
static OTEL_OTLP_ENDPOINT: ParameterStr = ParameterStr::new(None); // populated during [define]
static OTEL_OTLP_PROTOCOL: ParameterStr = ParameterStr::new(None); // populated during [define]
static OTEL_OTLP_TIMEOUT_MS: ParameterInt = ParameterInt::new(1); // populated during [define]

// These static variables are populated by our GUC assign hooks during ProcessConfigFile.

static PARSED_EXPORTS: LazyLock<RwLock<ExportSignalSet>> =
    LazyLock::new(|| RwLock::new(ExportSignalSet::empty()));

#[allow(dead_code)]
pub fn exporter() -> (
    ExportProtocol,
    ExportEndpoint,
    Option<ExportCompression>,
    Duration,
    HashMap<String, String>,
) {
    let compression = match OTEL_OTLP_COMPRESSION.get() {
        None => None,
        Some(v) if v.is_empty() => None,
        Some(v) => Some(
            ExportCompression::try_from(v.as_c_str()).expect("check ensures this is valid text"),
        ),
    };

    let endpoint = match OTEL_OTLP_ENDPOINT.get() {
        None => otlp::OTEL_EXPORTER_OTLP_ENDPOINT_DEFAULT.parse().unwrap(),
        Some(v) => {
            ExportEndpoint::try_from(v.as_c_str()).expect("check ensures this is valid text")
        }
    };

    let metadata = HashMap::new();

    let protocol = match OTEL_OTLP_PROTOCOL.get() {
        None => otlp::OTEL_EXPORTER_OTLP_PROTOCOL_DEFAULT.parse().unwrap(),
        Some(v) => {
            ExportProtocol::try_from(v.as_c_str()).expect("check ensures this is valid text")
        }
    };

    let timeout = match OTEL_OTLP_TIMEOUT_MS.get() {
        n if n > 0 => Duration::from_millis(n as u64),
        _ => otlp::OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT,
    };

    (protocol, endpoint, compression, timeout, metadata)
}

/// Returns true when signal is present in the "otel.export" GUC variable.
pub fn exporting(signal: ExportSignal) -> bool {
    match PARSED_EXPORTS.read() {
        Ok(exports) => exports.contains(signal),
        Err(_) => false,
    }
}

#[allow(dead_code)]
enum GucHookError {
    ErrCode(i32),
    Message(CString),
    Detail(CString),
    Hint(CString),
}

fn guc_check_hook_error(args: &[GucHookError]) {
    // https://doxygen.postgresql.org/guc_8h.html

    for arg in args {
        match arg {
            GucHookError::ErrCode(code) => unsafe {
                pg_sys::GUC_check_errcode(*code);
            },
            GucHookError::Message(text) => unsafe {
                // Do the work of GUC_check_errmsg.
                pg_sys::pre_format_elog_string(0, std::ptr::null());
                pg_sys::GUC_check_errmsg_string = pg_sys::format_elog_string(text.as_ptr());
            },
            GucHookError::Detail(text) => unsafe {
                // Do the work of GUC_check_errdetail.
                pg_sys::pre_format_elog_string(0, std::ptr::null());
                pg_sys::GUC_check_errdetail_string = pg_sys::format_elog_string(text.as_ptr());
            },
            GucHookError::Hint(text) => unsafe {
                // Do the work of GUC_check_errhint.
                pg_sys::pre_format_elog_string(0, std::ptr::null());
                pg_sys::GUC_check_errhint_string = pg_sys::format_elog_string(text.as_ptr());
            },
        }
    }
}

pub fn define() {
    use pgrx::{GucContext, GucFlags, GucRegistry, pg_sys::GucSource};

    const CTX_SERVER_CONFIG: GucContext = GucContext::Sighup; // startup or config; requires reload
    const CTX_SHOW_ONLY: GucContext = GucContext::Internal; // cannot be set, only shown

    const FLAGS_LIST: GucFlags = GucFlags::from_bits_retain(pg_sys::GUC_LIST_INPUT as i32);
    const FLAGS_NAME: GucFlags = GucFlags::IS_NAME;
    const FLAGS_NONE: GucFlags = GucFlags::empty();

    GucRegistry::define_int_guc(
        c"otel.attribute_count_limit",                // name
        c"Maximum attributes allowed on each signal", // short
        c"",                                          // long
        &OTEL_ATTRIBUTE_COUNT_LIMIT,
        OTEL_ATTRIBUTE_COUNT_LIMIT.get(), // min
        OTEL_ATTRIBUTE_COUNT_LIMIT.get(), // max
        CTX_SHOW_ONLY,
        FLAGS_NONE,
    );

    // SAFETY: GUC hooks *must* be defined with #[pg_guard].
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.export",                 // name
            c"Signals to export over OTLP", // short
            c"May be empty or \"logs\".",   // long
            &OTEL_EXPORTS,
            CTX_SERVER_CONFIG,
            FLAGS_LIST | FLAGS_NAME,
            Some(check),  // check
            Some(assign), // assign
            None,         // show
        );

        /// Called when a GUC value is proposed.
        #[pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            match ExportSignalSet::from_ptr(&raw) {
                Ok(_) => true,
                Err(err) => {
                    guc_check_hook_error(&[GucHookError::Detail(
                        CString::new(err.to_string()).unwrap(),
                    )]);
                    return false;
                }
            }
        }

        /// Called after all proposed GUC values are valid.
        #[pg_guard]
        extern "C-unwind" fn assign(next: *const c_char, _extra: pgrx::void_mut_ptr) {
            let mut singleton = PARSED_EXPORTS.write().unwrap();

            *singleton = if next.is_null() {
                ExportSignalSet::empty()
            } else {
                ExportSignalSet::try_from(unsafe { CStr::from_ptr(next) })
                    .expect("check ensures this is valid text")
            };
        }
    }

    // SAFETY: GUC hooks *must* be defined with #[pg_guard].
    #[cfg(any(feature = "gzip", feature = "zstd"))]
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.otlp_compression",                             // name
            c"Compression with which the exporter sends signals", // short
            #[cfg(all(feature = "gzip", feature = "zstd"))]
            c"May be empty, \"gzip\" or \"zstd\".", // long
            #[cfg(all(feature = "gzip", not(feature = "zstd")))]
            c"May be empty or \"gzip\".", // long
            #[cfg(all(not(feature = "gzip"), feature = "zstd"))]
            c"May be empty or \"zstd\".", // long
            &OTEL_OTLP_COMPRESSION,
            CTX_SERVER_CONFIG,
            FLAGS_NONE,
            Some(check), // check
            None,        // assign
            None,        // show
        );

        /// Called when a GUC value is proposed.
        #[pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            match ExportCompression::from_ptr(&raw) {
                Ok(None) => true,
                Ok(_) => true,
                Err(err) => {
                    guc_check_hook_error(&[GucHookError::Detail(
                        CString::new(err.to_string()).unwrap(),
                    )]);
                    return false;
                }
            }
        }
    }

    // SAFETY: GUC hooks *must* be defined with #[pg_guard].
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.otlp_endpoint",                               // name
            c"Target URL to which the exporter sends signals",   // short
            c"A scheme of https indicates a secure connection.", // long
            &OTEL_OTLP_ENDPOINT,
            CTX_SERVER_CONFIG,
            FLAGS_NONE,
            Some(check), // check
            None,        // assign
            None,        // show
        );

        /// Called when a GUC value is proposed.
        #[pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            match ExportEndpoint::from_ptr(&raw) {
                // Allow null only during initialiazation
                Ok(None) => unsafe { pg_sys::process_shared_preload_libraries_in_progress },
                Ok(_) => true,
                Err(err) => {
                    guc_check_hook_error(&[GucHookError::Detail(
                        CString::new(err.to_string()).unwrap(),
                    )]);
                    return false;
                }
            }
        }
    }

    // SAFETY: GUC hooks *must* be defined with #[pg_guard].
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.otlp_protocol",              // name
            c"The exporter transport protocol", // short
            c"",                                // long
            &OTEL_OTLP_PROTOCOL,
            CTX_SERVER_CONFIG,
            FLAGS_NONE,
            Some(check), // check
            None,        // assign
            None,        // show
        );

        /// Called when a GUC value is proposed.
        #[pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            match ExportProtocol::from_ptr(&raw) {
                // Allow null only during initialiazation
                Ok(None) => unsafe { pg_sys::process_shared_preload_libraries_in_progress },
                Ok(_) => true,
                Err(err) => {
                    guc_check_hook_error(&[GucHookError::Detail(
                        CString::new(err.to_string()).unwrap(),
                    )]);
                    return false;
                }
            }
        }
    }

    GucRegistry::define_int_guc(
        c"otel.otlp_timeout",                                         // name
        c"Maximum time the exporter will wait for each batch export", // short
        c"",                                                          // long
        &OTEL_OTLP_TIMEOUT_MS,
        1,                 // min = 1ms
        60 * 60 * 1000,    // max = 60min
        CTX_SERVER_CONFIG, //
        GucFlags::UNIT_MS, // milliseconds
    );

    //todo!("string, hooks: otel.resource_attributes");

    // SAFETY: GUC hooks *must* be defined with #[pg_guard].
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.service_name",            // name
            c"Logical name of this service", // short
            c"",                             // long
            &OTEL_SERVICE_NAME,
            CTX_SERVER_CONFIG,
            FLAGS_NONE,
            Some(check), // check
            None,        // assign
            None,        // show
        );

        /// Called when a GUC value is proposed.
        #[pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            if raw.is_null() || unsafe { CStr::from_ptr(raw) }.is_empty() {
                guc_check_hook_error(&[GucHookError::Detail(
                    CString::new(format!(
                        "resource attribute {:?} cannot be blank.",
                        "service.name"
                    ))
                    .unwrap(),
                )]);
                return false;
            }

            match unsafe { CStr::from_ptr(raw) }.to_str() {
                Ok(_) => true,
                Err(err) => {
                    guc_check_hook_error(&[GucHookError::Detail(
                        CString::new(err.to_string()).unwrap(),
                    )]);
                    return false;
                }
            }
        }
    }

    // Do not allow any other GUCs starting with "otel."
    // This ensures we can define more GUCs in the future.
    unsafe {
        #[cfg(any(feature = "pg13", feature = "pg14"))]
        pg_sys::EmitWarningsOnPlaceholders(c"otel".as_ptr());

        #[cfg(not(any(feature = "pg13", feature = "pg14")))]
        pg_sys::MarkGUCPrefixReserved(c"otel".as_ptr());
    }

    // Assign defaults compiled into OpenTelemetry crates.
    // SAFETY: SetConfigOption copies its inputs into a GUC memory context.
    unsafe {
        pg_sys::SetConfigOption(
            c"otel.otlp_endpoint".as_ptr(),
            CString::new(otlp::OTEL_EXPORTER_OTLP_ENDPOINT_DEFAULT)
                .unwrap()
                .as_ptr(),
            pg_sys::GucContext::PGC_POSTMASTER,
            pg_sys::GucSource::PGC_S_DEFAULT,
        );
        pg_sys::SetConfigOption(
            c"otel.otlp_protocol".as_ptr(),
            CString::new(otlp::OTEL_EXPORTER_OTLP_PROTOCOL_DEFAULT)
                .unwrap()
                .as_ptr(),
            pg_sys::GucContext::PGC_POSTMASTER,
            pg_sys::GucSource::PGC_S_DEFAULT,
        );
        pg_sys::SetConfigOption(
            c"otel.otlp_timeout".as_ptr(),
            CString::new(
                otlp::OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT
                    .as_millis()
                    .to_string(),
            )
            .unwrap()
            .as_ptr(),
            pg_sys::GucContext::PGC_POSTMASTER,
            pg_sys::GucSource::PGC_S_DEFAULT,
        );
    }

    // Read OpenTelemetry configuration from the environment.
    {
        // Assign GUC option when environment variable key exists and contains a value.
        //
        // https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables#parsing-empty-value
        //
        // > The SDK MUST interpret an empty value of an environment variable the same way as when the variable is unset.
        //
        fn read(option: &CStr, key: &str) {
            let Ok(value) = std::env::var(key) else {
                return;
            };
            if value.is_empty() {
                return;
            }
            let Ok(cstr) = CString::new(value) else {
                return;
            };

            // SAFETY: SetConfigOption copies its inputs into a GUC memory context.
            unsafe {
                pg_sys::SetConfigOption(
                    option.as_ptr(),
                    cstr.as_ptr(),
                    pg_sys::GucContext::PGC_POSTMASTER,
                    pg_sys::GucSource::PGC_S_ENV_VAR,
                );
            }
        }

        // https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables#attribute-limits
        read(c"otel.attribute_count_limit", "OTEL_ATTRIBUTE_COUNT_LIMIT");

        // https://opentelemetry.io/docs/specs/otel/protocol/exporter
        read(c"otel.otlp_endpoint", "OTEL_EXPORTER_OTLP_ENDPOINT");
        read(c"otel.otlp_protocol", "OTEL_EXPORTER_OTLP_PROTOCOL");
        read(c"otel.otlp_timeout", "OTEL_EXPORTER_OTLP_TIMEOUT");

        // https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables#general-sdk-configuration
        // "OTEL_SDK_DISABLED=true" should no-op all telemetry signals
        if std::env::var("OTEL_SDK_DISABLED").is_ok_and(|v| v == "true") {
            // SAFETY: SetConfigOption copies its inputs into a GUC memory context.
            unsafe {
                pg_sys::SetConfigOption(
                    c"otel.export".as_ptr(),
                    std::ptr::null(),
                    pg_sys::GucContext::PGC_POSTMASTER,
                    pg_sys::GucSource::PGC_S_ENV_VAR,
                );
            }
        }
        read(c"otel.resource_attributes", "OTEL_RESOURCE_ATTRIBUTES");
        read(c"otel.service_name", "OTEL_SERVICE_NAME");
    }
}
