// SPDX-License-Identifier: ISC

use super::FromStr;
use opentelemetry_otlp as otlp;
use opentelemetry_sdk as sdk;
use pgrx::pg_sys;
use std::ffi::{CStr, CString, c_char};
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

type ParameterInt = pgrx::GucSetting<i32>;
type ParameterStr = pgrx::GucSetting<Option<CString>>;

// These static variables are populated by PostgreSQL during ProcessConfigFile.
// - https://doxygen.postgresql.org/guc_8h.html

static OTEL_ATTRIBUTE_COUNT_LIMIT: ParameterInt = ParameterInt::new(128);
static OTEL_EXPORTS: ParameterStr = ParameterStr::new(None);
static OTEL_RESOURCE_ATTRIBUTES: ParameterStr = ParameterStr::new(None);
static OTEL_SERVICE_NAME: ParameterStr = ParameterStr::new(Some(c"postgresql"));

static OTEL_OTLP_COMPRESSION: ParameterStr = ParameterStr::new(None);
static OTEL_OTLP_ENDPOINT: ParameterStr = ParameterStr::new(None); // populated during [`define_guc_variables()`]
static OTEL_OTLP_HEADERS: ParameterStr = ParameterStr::new(None);
static OTEL_OTLP_PROTOCOL: ParameterStr = ParameterStr::new(None); // populated during [`define_guc_variables()`]
static OTEL_OTLP_TIMEOUT_MS: ParameterInt = ParameterInt::new(1); // populated during [`define_guc_variables()`]

// These static variables are populated by our GUC assign hooks during ProcessConfigFile.

static PARSED_EXPORTS: LazyLock<RwLock<super::ExportSignalSet>> =
    LazyLock::new(|| RwLock::new(super::ExportSignalSet::empty()));

/// Returns true when signal is present in the `otel.export` GUC variable.
/// This is faster than [`loaded()`] and safe to call from anywhere.
pub fn exporting(signal: super::ExportSignal) -> bool {
    match PARSED_EXPORTS.read() {
        Ok(exports) => exports.contains(signal),
        _ => false,
    }
}

/// This returns a copy of all configuration.
///
/// # Safety
///
/// This can only be called from the main thread because it reads from Postgres GUC variables.
pub unsafe fn loaded() -> super::Config {
    let export = super::OTLP {
        compression: match OTEL_OTLP_COMPRESSION.get() {
            None => None,
            Some(v) if v.is_empty() => None,
            Some(v) => Some(
                super::ExportCompression::try_from(v.as_c_str())
                    .unwrap()
                    .into(),
            ),
        },
        endpoint: match OTEL_OTLP_ENDPOINT.get() {
            None => otlp::OTEL_EXPORTER_OTLP_ENDPOINT_DEFAULT.into(),
            Some(v) => super::ExportEndpoint::try_from(v.as_c_str())
                .unwrap()
                .to_string(),
        },
        headers: match OTEL_OTLP_HEADERS.get() {
            None => http::HeaderMap::new(),
            Some(v) => super::Baggage::try_from(v.as_c_str())
                .unwrap()
                .try_into()
                .unwrap(),
        },
        protocol: match OTEL_OTLP_PROTOCOL.get() {
            None => otlp::OTEL_EXPORTER_OTLP_PROTOCOL_DEFAULT
                .parse::<super::ExportProtocol>()
                .unwrap()
                .into(),
            Some(v) => super::ExportProtocol::try_from(v.as_c_str())
                .unwrap()
                .into(),
        },
        timeout: match OTEL_OTLP_TIMEOUT_MS.get() {
            n if n > 0 => Duration::from_millis(n as u64),
            _ => otlp::OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT,
        },
    };

    // TODO: with_schema_url()

    // Start with only the SDK name and version.
    let mut resource = sdk::Resource::builder_empty()
        .with_detector(Box::new(sdk::resource::TelemetryResourceDetector));

    if let Some(v) = OTEL_RESOURCE_ATTRIBUTES.get() {
        resource = resource.with_attributes(super::Baggage::try_from(v.as_c_str()).unwrap());
    }

    if let Some(cstr) = OTEL_SERVICE_NAME.get() {
        resource = resource.with_service_name(cstr.clone().into_string().unwrap());
    }

    super::Config {
        logs_otlp: export,
        resource: resource.build(),
    }
}

struct HookError;
impl HookError {
    #[allow(dead_code)]
    fn errcode(code: std::ffi::c_int) {
        unsafe { pg_sys::GUC_check_errcode(code) };
    }

    fn detail(text: CString) {
        unsafe {
            // Do the work of GUC_check_errdetail.
            pg_sys::pre_format_elog_string(0, std::ptr::null());
            pg_sys::GUC_check_errdetail_string = pg_sys::format_elog_string(text.as_ptr());
        }
    }

    #[allow(dead_code)]
    fn hint(text: CString) {
        unsafe {
            // Do the work of GUC_check_errhint.
            pg_sys::pre_format_elog_string(0, std::ptr::null());
            pg_sys::GUC_check_errhint_string = pg_sys::format_elog_string(text.as_ptr());
        }
    }

    #[allow(dead_code)]
    fn message(text: CString) {
        unsafe {
            // Do the work of GUC_check_errmsg.
            pg_sys::pre_format_elog_string(0, std::ptr::null());
            pg_sys::GUC_check_errmsg_string = pg_sys::format_elog_string(text.as_ptr());
        }
    }
}

pub fn define_guc_variables() {
    debug_assert!(crate::assert_postmaster_startup());

    // Assign compile-time defaults stored in OpenTelemetry crates.
    // [`GucRegistry`] expects static variables contain their default value when being registered.
    //
    // SAFETY: These pointers refer to values inside these static variables and are safe to dereference.
    unsafe {
        *OTEL_OTLP_ENDPOINT.as_ptr() =
            pgrx::StringInfo::from(otlp::OTEL_EXPORTER_OTLP_ENDPOINT_DEFAULT)
                .into_char_ptr()
                .cast_mut();

        *OTEL_OTLP_PROTOCOL.as_ptr() =
            pgrx::StringInfo::from(otlp::OTEL_EXPORTER_OTLP_PROTOCOL_DEFAULT)
                .into_char_ptr()
                .cast_mut();

        *OTEL_OTLP_TIMEOUT_MS.as_ptr() =
            otlp::OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT.as_millis() as i32;
    }

    use pgrx::{GucContext, GucFlags, GucRegistry, pg_sys::GucSource};

    struct Context;
    impl Context {
        /// startup or config; requires reload
        const SERVER_RELOAD: GucContext = GucContext::Sighup;
        /// startup or config; requires restart
        const _SERVER_RESTART: GucContext = GucContext::Postmaster;
        /// cannot be set, only shown
        const SHOW_ONLY: GucContext = GucContext::Internal;
    }

    struct Options;
    impl Options {
        /// input can be in list format
        const LIST: GucFlags = GucFlags::from_bits_retain(pg_sys::GUC_LIST_INPUT as i32);
        /// limit string length to NAMEDATALEN-1
        const NAME: GucFlags = GucFlags::IS_NAME;
        const NONE: GucFlags = GucFlags::empty();
        /// number in milliseconds
        const UNIT_MS: GucFlags = GucFlags::UNIT_MS;
    }

    GucRegistry::define_int_guc(
        c"otel.attribute_count_limit",                // name
        c"Maximum attributes allowed on each signal", // short
        c"",                                          // long
        &OTEL_ATTRIBUTE_COUNT_LIMIT,
        OTEL_ATTRIBUTE_COUNT_LIMIT.get(), // min
        OTEL_ATTRIBUTE_COUNT_LIMIT.get(), // max
        Context::SHOW_ONLY,
        Options::NONE,
    );

    // SAFETY: GUC hooks *must* be defined with the [`pgrx::pg_guard`] attribute.
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.export",                 // name
            c"Signals to export over OTLP", // short
            cr#"May be empty or "logs"."#,  // long
            &OTEL_EXPORTS,
            Context::SERVER_RELOAD,
            Options::LIST | Options::NAME,
            Some(check),  // check
            Some(assign), // assign
            None,         // show
        );

        /// Called when a GUC value is proposed.
        #[pgrx::pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            if let Err(err) = super::ExportSignalSet::try_from_ptr(&raw) {
                HookError::detail(CString::new(err.to_string()).unwrap());
                false
            } else {
                true
            }
        }

        /// Called after all proposed GUC values are valid.
        #[pgrx::pg_guard]
        extern "C-unwind" fn assign(next: *const c_char, _extra: pgrx::void_mut_ptr) {
            let mut singleton = PARSED_EXPORTS.write().unwrap();

            *singleton = super::ExportSignalSet::try_from_ptr(&next)
                .expect("check ensures this is valid text");
        }
    }

    // SAFETY: GUC hooks *must* be defined with the [`pgrx::pg_guard`] attribute.
    #[cfg(any(feature = "gzip", feature = "zstd"))]
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.otlp_compression",                             // name
            c"Compression with which the exporter sends signals", // short
            #[cfg(all(feature = "gzip", feature = "zstd"))]
            cr#"May be empty, "gzip" or "zstd"."#, // long
            #[cfg(all(feature = "gzip", not(feature = "zstd")))]
            cr#"May be empty or "gzip"."#, // long
            #[cfg(all(not(feature = "gzip"), feature = "zstd"))]
            cr#"May be empty or "zstd"."#, // long
            &OTEL_OTLP_COMPRESSION,
            Context::SERVER_RELOAD,
            Options::NONE,
            Some(check), // check
            None,        // assign
            None,        // show
        );

        /// Called when a GUC value is proposed.
        #[pgrx::pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            if let Err(err) = super::ExportCompression::try_from_ptr(&raw) {
                HookError::detail(CString::new(err.to_string()).unwrap());
                false
            } else {
                true
            }
        }
    }

    // SAFETY: GUC hooks *must* be defined with the [`pgrx::pg_guard`] attribute.
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.otlp_endpoint",                               // name
            c"Target URL to which the exporter sends signals",   // short
            c"A scheme of https indicates a secure connection.", // long
            &OTEL_OTLP_ENDPOINT,
            Context::SERVER_RELOAD,
            Options::NONE,
            Some(check), // check
            None,        // assign
            None,        // show
        );

        /// Called when a GUC value is proposed.
        #[pgrx::pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            match super::ExportEndpoint::try_from_ptr(&raw) {
                // Allow null only during initialiazation
                Ok(None) => unsafe { pg_sys::process_shared_preload_libraries_in_progress },
                Ok(_) => true,
                Err(err) => {
                    HookError::detail(CString::new(err.to_string()).unwrap());
                    false
                }
            }
        }
    }

    // SAFETY: GUC hooks *must* be defined with the [`pgrx::pg_guard`] attribute.
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.otlp_headers",                        // name
            c"Key-value pairs included in OTLP headers", // short
            c"Formatted as W3C Baggage",                 // long
            &OTEL_OTLP_HEADERS,
            Context::SERVER_RELOAD,
            Options::NONE,
            Some(check), // check
            None,        // assign
            None,        // show
        );

        /// Called when a GUC value is proposed.
        #[pgrx::pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            match super::Baggage::try_from_ptr(&raw).map(TryInto::<http::HeaderMap>::try_into) {
                Ok(_) => true,
                Err(err) => {
                    HookError::detail(CString::new(err.to_string()).unwrap());
                    false
                }
            }
        }
    }

    // SAFETY: GUC hooks *must* be defined with the [`pgrx::pg_guard`] attribute.
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.otlp_protocol",              // name
            c"The exporter transport protocol", // short
            c"",                                // long
            &OTEL_OTLP_PROTOCOL,
            Context::SERVER_RELOAD,
            Options::NONE,
            Some(check), // check
            None,        // assign
            None,        // show
        );

        /// Called when a GUC value is proposed.
        #[pgrx::pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            match super::ExportProtocol::try_from_ptr(&raw) {
                // Allow null only during initialiazation
                Ok(None) => unsafe { pg_sys::process_shared_preload_libraries_in_progress },
                Ok(_) => true,
                Err(err) => {
                    HookError::detail(CString::new(err.to_string()).unwrap());
                    false
                }
            }
        }
    }

    GucRegistry::define_int_guc(
        c"otel.otlp_timeout",                                         // name
        c"Maximum time the exporter will wait for each batch export", // short
        c"",                                                          // long
        &OTEL_OTLP_TIMEOUT_MS,
        1,                      // min = 1ms
        60 * 60 * 1000,         // max = 60min
        Context::SERVER_RELOAD, //
        Options::UNIT_MS,       // milliseconds
    );

    // SAFETY: GUC hooks *must* be defined with the [`pgrx::pg_guard`] attribute.
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.resource_attributes",                          // name
            c"Key-value pairs to be used as resource attributes", // short
            c"Formatted as W3C Baggage",                          // long
            &OTEL_RESOURCE_ATTRIBUTES,
            Context::SERVER_RELOAD,
            Options::NONE,
            Some(check), // check
            None,        // assign
            None,        // show
        );

        /// Called when a GUC value is proposed.
        #[pgrx::pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            match super::Baggage::try_from_ptr(&raw) {
                Ok(_) => true,
                Err(err) => {
                    HookError::detail(CString::new(err.to_string()).unwrap());
                    false
                }
            }
        }
    }

    // SAFETY: GUC hooks *must* be defined with the [`pgrx::pg_guard`] attribute.
    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.service_name",            // name
            c"Logical name of this service", // short
            c"",                             // long
            &OTEL_SERVICE_NAME,
            Context::SERVER_RELOAD,
            Options::NONE,
            Some(check), // check
            None,        // assign
            None,        // show
        );

        /// Called when a GUC value is proposed.
        #[pgrx::pg_guard]
        extern "C-unwind" fn check(
            next: *mut *mut c_char,
            _extra: *mut pgrx::void_mut_ptr,
            _source: GucSource::Type,
        ) -> bool {
            debug_assert!(!next.is_null(), "expected value in check hook");

            // SAFETY: dereference is safe because the pointer is never null.
            let raw: *const c_char = unsafe { *next };

            if raw.is_null() || unsafe { CStr::from_ptr(raw) }.is_empty() {
                HookError::detail(
                    CString::new(r#"resource attribute "service.name" cannot be blank"#).unwrap(),
                );
                return false;
            }

            if let Err(err) = unsafe { CStr::from_ptr(raw) }.to_str() {
                HookError::detail(CString::new(err.to_string()).unwrap());
                false
            } else {
                true
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
        read(c"otel.otlp_headers", "OTEL_EXPORTER_OTLP_HEADERS");
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
