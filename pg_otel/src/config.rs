// SPDX-License-Identifier: MIT

//! Configuration layer for PostgreSQL GUC reads and writes.
//!
//! # Architecture & Encapsulation
//!
//! PostgreSQL exposes server, session, and transaction parameters through Grand Unified
//! Configuration (GUC) definitions backed by static variables. Only a few parameter types
//! are supported: text (null-terminated), integer (i32), and boolean.
//!
//! This module is intended to manage all the GUC parameters for this crate and provide a read-only
//! interface to their values. Getter functions safely access global state, minimize allocations
//! when converting strings, and adapt C primitives into strongly-typed domain values.
//!
//! # C GUC Types vs. Rust Domain Mappings
//!
//! Because PostgreSQL parameters are limited types, this module acts as a semantic boundary that
//! validates and adapts raw values into rich Rust representations:
//! - **Unsigned Counts & Durations**: Quantities that are naturally non-negative are bounded at
//!   registration and exposed as [`u32`], [`usize`], or [`time::Duration`].
//! - **Structured Types**: Complex text parameters are validated by GUC hooks and exposed as parsed
//!   abstractions, such as [`url::Url`] and [`reqwest::header::HeaderMap`].
//! - **Lookup Caching**: Comma-separated lists are parsed once in an assign hook and cached as
//!   thread-local booleans for zero-cost $O(1)$ checks in backend hooks.
//!
//! # Zero-Allocation String Handling
//!
//! Standard `pgrx::guc::GucSetting<Option<CString>>::get()` clones the value onto the heap.
//! Instead:
//! - [`GucStringExt::as_cstr`] borrows the pointer directly from PostgreSQL's GUC memory as a `&CStr`.
//! - [`with_stack_cstr`] copies strings into a stack buffer for null-terminated parsing, safely
//!   falling back to a temporary `CString` allocation when needed.

use crate::PG_OTEL_LIBRARY;
use core::cell::Cell;
use pgrx::pg_sys;
use std::{ffi, time};
use url::Url as URL;

type GucBool = pgrx::guc::GucSetting<bool>;
type GucInt32 = pgrx::guc::GucSetting<i32>;
type GucString = pgrx::guc::GucSetting<Option<ffi::CString>>;

/// Extension methods for [`GucString`].
trait GucStringExt {
    /// Borrows the current C string from a [`GucString`] without heap allocation.
    ///
    /// Returns `None` if the pointer is null or points to an empty string.
    fn as_cstr(&self) -> Option<&ffi::CStr>;

    /// Sets the named GUC using a stack buffer for null-termination, falling back to heap
    /// allocation only when `value` exceeds `N` bytes.
    fn set<const N: usize>(
        name: &ffi::CStr,
        value: &str,
        context: pg_sys::GucContext::Type,
        source: pg_sys::GucSource::Type,
        elevel: i32,
    ) {
        with_stack_cstr::<N, _>(value, |cstr| unsafe {
            pg_sys::set_config_option(
                name.as_ptr(),
                cstr.as_ptr(),
                context,
                source,
                pg_sys::GucAction::GUC_ACTION_SET,
                true, // validate then assign
                elevel,
                false, // not part of a reload
            );
        });
    }
}

impl GucStringExt for GucString {
    fn as_cstr(&self) -> Option<&ffi::CStr> {
        // `GucSetting::get()` returns a cloned `ffi::CString`.
        unsafe {
            let v = *self.as_ptr();
            if v.is_null() || *v == 0 {
                None
            } else {
                Some(ffi::CStr::from_ptr(v))
            }
        }
    }
}

static GUC_BATCH_MAX_DELAY_MS: GucInt32 = GucInt32::new(1000);
static GUC_BATCH_MAX_ITEMS: GucInt32 = GucInt32::new(512);
static GUC_COMPRESSION: GucString = GucString::new(Some(c"none"));
static GUC_ENDPOINT: GucString = GucString::new(Some(c"http://localhost:4318"));
static GUC_EXPORT_LOGS: GucBool = GucBool::new(false);
static GUC_EXPORT_TRACES: GucBool = GucBool::new(false);
static GUC_HEADERS: GucString = GucString::new(None);
static GUC_MAX_NESTED_STATEMENTS: GucInt32 = GucInt32::new(50);
static GUC_PROTOCOL: GucString = GucString::new(Some(c"http/protobuf"));
static GUC_RESOURCE_ATTRIBUTES: GucString = GucString::new(None);
static GUC_SERVICE_NAME: GucString = GucString::new(Some(c"postgresql"));
static GUC_SQLCOMMENTER: GucBool = GucBool::new(true);
static GUC_TIMEOUT_MS: GucInt32 = GucInt32::new(10000);
static GUC_TRACE_NESTED_STATEMENTS: GucInt32 = GucInt32::new(0);
static GUC_TRACE_PARALLEL_WORKERS: GucBool = GucBool::new(false);
static GUC_TRACE_STATEMENTS: GucBool = GucBool::new(true);
static GUC_TRACE_TRANSACTIONS: GucBool = GucBool::new(true);
static GUC_TRACEPARENT: GucString = GucString::new(None);
static GUC_TRACESTATE: GucString = GucString::new(None);

thread_local! {
    static UPDATING_SELF: Cell<bool> = const { Cell::new(false) };
}

/// Registers all `otel.*` configuration options in PostgreSQL's GUC registry during `_PG_init`.
pub fn define_guc_variables() {
    use pg_sys::GucSource;
    use pgrx::guc::{GucCheckError, GucContext, GucFlags, GucRegistry};

    /// Constructs a [`GucCheckError`] with information in `DETAIL` rather than overriding
    /// PostgreSQL's localized "invalid value for parameter" message.
    #[inline]
    fn check_error(detail: impl std::fmt::Display) -> GucCheckError {
        GucCheckError::default().with_detail(detail.to_string())
    }

    GucRegistry::define_bool_guc(
        c"otel.export_logs",
        c"Export log records",
        c"Controls whether error and log messages are exported.",
        &GUC_EXPORT_LOGS,
        GucContext::Suset, // superuser or granted role can set for session, user, database
        GucFlags::empty(), // visible to everyone
    );

    GucRegistry::define_bool_guc(
        c"otel.export_traces",
        c"Export trace spans",
        c"Controls whether trace spans are exported.",
        &GUC_EXPORT_TRACES,
        GucContext::Suset, // superuser or granted role can set for session, user, database
        GucFlags::empty(), // visible to everyone
    );

    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.otlp_endpoint",
            c"URL to which the exporter sends telemetry",
            c"Base URL of an OpenTelemetry collector.",
            &GUC_ENDPOINT,
            GucContext::Sighup, // config file, affects entire server, requires reload
            GucFlags::empty(),  // visible to everyone
            Some(check_endpoint),
            None,
            None,
        );

        #[pgrx::pg_guc_hook(check)]
        fn check_endpoint(value: Option<ffi::CString>) -> Result<(), GucCheckError> {
            eyre::OptionExt::ok_or_eyre(value.as_deref(), "required")
                .and_then(Endpoint::validate)
                .map_err(check_error)
        }
    }

    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.otlp_headers",
            c"OTLP request headers (superuser only)",
            c"Headers sent with OTLP export requests. May contain sensitive credentials. Restricted to superusers.",
            &GUC_HEADERS,
            GucContext::Sighup, // config file, affects entire server, requires reload
            GucFlags::SUPERUSER_ONLY, // requires `pg_read_all_settings` role
            Some(check_headers),
            None,
            None,
        );

        #[pgrx::pg_guc_hook(check)]
        fn check_headers(value: Option<ffi::CString>) -> Result<(), GucCheckError> {
            if let Some(value) = value {
                Headers::validate(&value).map_err(check_error)?;
            }
            Ok(())
        }
    }

    GucRegistry::define_string_guc(
        c"otel.otlp_protocol",
        c"OTLP transport protocol",
        c"Exporter protocol (http/protobuf).",
        &GUC_PROTOCOL,
        GucContext::Internal, // read-only; informational
        GucFlags::empty(),    // visible to everyone
    );

    GucRegistry::define_int_guc(
        c"otel.otlp_batch_max_items",
        c"Max items per OTLP export batch",
        c"Maximum number of logs or spans exported in a single OTLP batch.",
        &GUC_BATCH_MAX_ITEMS,
        1,
        i32::MAX / 4,
        GucContext::Sighup, // config file, affects entire server, requires reload
        GucFlags::empty(),  // visible to everyone
    );
    GucRegistry::define_int_guc(
        c"otel.otlp_batch_max_delay",
        c"Max wait time before exporting a batch",
        c"Maximum duration (in milliseconds) the exporter waits before flushing.",
        &GUC_BATCH_MAX_DELAY_MS,
        0,
        time::Duration::from_hours(1).as_millis() as i32,
        GucContext::Sighup, // config file, affects entire server, requires reload
        GucFlags::UNIT_MS,  // visible to everyone
    );
    GucRegistry::define_int_guc(
        c"otel.otlp_timeout",
        c"Maximum time to deliver an export batch",
        c"Maximum time allowed to contact the collector, send a batch, and receive an acknowledgment, including all request attempts and retries.",
        &GUC_TIMEOUT_MS,
        1,
        i32::MAX,
        GucContext::Sighup, // config file, affects entire server, requires reload
        GucFlags::UNIT_MS,  // visible to everyone
    );
    GucRegistry::define_string_guc(
        c"otel.otlp_compression",
        c"HTTP payload compression",
        c"Payload compression algorithm (none, gzip, or zstd).",
        &GUC_COMPRESSION,
        GucContext::Sighup, // config file, affects entire server, requires reload
        GucFlags::empty(),  // visible to everyone
    );

    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.resource_attributes",
            c"Instance metadata attached to all telemetry",
            c"Comma-separated key=value pairs identifying this Postgres instance and environment. Attached to all exported telemetry. Keys and values may be percent-encoded.",
            &GUC_RESOURCE_ATTRIBUTES,
            GucContext::Sighup, // config file, affects entire server, requires reload
            GucFlags::empty(),  // visible to everyone
            Some(check_resource_attributes),
            None,
            None,
        );

        #[pgrx::pg_guc_hook(check)]
        fn check_resource_attributes(value: Option<ffi::CString>) -> Result<(), GucCheckError> {
            if let Some(value) = value {
                ResourceAttributes::validate(&value).map_err(check_error)?;
            }
            Ok(())
        }
    }

    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.service_name",
            c"Logical name of this service",
            c"Sets the 'service.name' resource attribute attached to all exported telemetry.",
            &GUC_SERVICE_NAME,
            GucContext::Sighup, // config file, affects entire server, requires reload
            GucFlags::empty(),  // visible to everyone
            Some(check_service_name),
            None,
            None,
        );

        #[pgrx::pg_guc_hook(check)]
        fn check_service_name(value: Option<ffi::CString>) -> Result<(), GucCheckError> {
            if let Some(value) = value {
                let s = value.to_str().map_err(check_error)?;
                if s.trim().is_empty() {
                    return Err(check_error("service name cannot be empty"));
                }
            }
            Ok(())
        }
    }

    GucRegistry::define_int_guc(
        c"otel.max_nested_statements",
        c"Maximum nested statement span limit",
        c"Maximum number of nested statement spans that may be emitted across a statement.",
        &GUC_MAX_NESTED_STATEMENTS,
        0,
        i32::MAX / 4,
        GucContext::Suset, // superuser or granted role can set for session, user, database
        GucFlags::empty(), // visible to everyone
    );

    GucRegistry::define_int_guc(
        c"otel.trace_nested_statements",
        c"Requested nested statement span budget",
        c"Requested number of nested statement spans to emit for procedures and triggers.",
        &GUC_TRACE_NESTED_STATEMENTS,
        0,
        i32::MAX / 4,
        GucContext::Userset, // self-service for everyone, any time
        GucFlags::empty(),   // visible to everyone
    );

    GucRegistry::define_bool_guc(
        c"otel.sqlcommenter",
        c"Parse SQLCommenter comments for trace context",
        c"Controls whether SQL query comments are inspected for distributed trace context.",
        &GUC_SQLCOMMENTER,
        GucContext::Suset, // superuser or granted role can set for session, user, database
        GucFlags::empty(), // visible to everyone
    );

    GucRegistry::define_bool_guc(
        c"otel.trace_parallel_workers",
        c"Trace parallel query workers",
        c"Controls whether parallel worker processes emit internal execution spans.",
        &GUC_TRACE_PARALLEL_WORKERS,
        GucContext::Suset, // superuser or granted role can set for session, user, database
        GucFlags::empty(), // visible to everyone
    );

    GucRegistry::define_bool_guc(
        c"otel.trace_statements",
        c"Trace SQL statement execution",
        c"Controls whether SQL statements are traced.",
        &GUC_TRACE_STATEMENTS,
        GucContext::Suset, // superuser or granted role can set for session, user, database
        GucFlags::empty(), // visible to everyone
    );

    GucRegistry::define_bool_guc(
        c"otel.trace_transactions",
        c"Trace database transactions",
        c"Controls whether transaction boundaries (BEGIN, COMMIT, ROLLBACK) are traced.",
        &GUC_TRACE_TRANSACTIONS,
        GucContext::Suset, // superuser or granted role can set for session, user, database
        GucFlags::empty(), // visible to everyone
    );

    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.traceparent",
            c"W3C traceparent context",
            c"W3C traceparent header for distributed tracing context propagation.",
            &GUC_TRACEPARENT,
            GucContext::Userset, // self-service for everyone, any time
            GucFlags::empty(),   // visible to everyone
            Some(check_traceparent),
            None,
            None,
        );

        #[pgrx::pg_guc_hook(check)]
        fn check_traceparent(
            value: Option<ffi::CString>,
            source: GucSource::Type,
        ) -> Result<(), GucCheckError> {
            let Some(v) = value else { return Ok(()) };
            let s = v.to_str().map_err(check_error)?;

            if !s.is_empty() {
                if !UPDATING_SELF.get() {
                    if source == GucSource::PGC_S_TEST {
                        return Err(check_error("cannot persist across restarts"));
                    }
                    if crate::context::depth() > 0 {
                        return Err(check_error("cannot set inside a nested statement"));
                    }
                }
                let headers = crate::context::Headers::new(s, None);
                crate::context::SpanContext::try_from(headers)
                    .map_err(|_| check_error("invalid traceparent format"))?;
            }
            Ok(())
        }
    }

    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.tracestate",
            c"W3C tracestate context",
            c"W3C tracestate header for distributed tracing context propagation.",
            &GUC_TRACESTATE,
            GucContext::Userset, // self-service for everyone, any time
            GucFlags::empty(),   // visible to everyone
            Some(check_tracestate),
            None,
            None,
        );

        #[pgrx::pg_guc_hook(check)]
        fn check_tracestate(
            value: Option<ffi::CString>,
            source: GucSource::Type,
        ) -> Result<(), GucCheckError> {
            let Some(v) = value else { return Ok(()) };
            let s = v.to_str().map_err(check_error)?;

            if !s.is_empty() && !UPDATING_SELF.get() {
                if source == GucSource::PGC_S_TEST {
                    return Err(check_error("cannot persist across restarts"));
                }
                if crate::context::depth() > 0 {
                    return Err(check_error("cannot set inside a nested statement"));
                }
            }
            Ok(())
        }
    }
}

/// Reads standard OpenTelemetry environment variables and applies non-blank values to GUCs.
///
/// PostgreSQL considers the origin of a value when assigning it to GUC variables. Values from
/// a source with higher precedence override those with a lower precedence:
///
/// 1. default values `PGC_S_DEFAULT` (lowest)
/// 2. environment variables `PGC_S_ENV_VAR`
/// 3. configuration files `PGC_S_FILE`
/// 4. command line arguments `PGC_S_ARGV`
///
/// This aligns with the OpenTelemetry specification, which has an explicit configuration file take
/// precedence over environment variables:
///
/// <https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables>
pub fn read_environment() {
    debug_assert!(crate::assert_postmaster_startup());

    // Set a GUC using PGC_POSTMASTER with PGC_S_ENV_VAR, the same as PostgreSQL's
    // `InitializeGUCOptionsFromEnvironment`. This has a higher precedence than default
    // values and lower precedence than `postgresql.conf` and `ALTER SYSTEM`.
    fn set<const N: usize>(name: &ffi::CStr, value: &str) {
        GucString::set::<N>(
            name,
            value,
            pg_sys::GucContext::PGC_POSTMASTER, // currently in postmaster startup
            pg_sys::GucSource::PGC_S_ENV_VAR,   // override default, overridden by file
            pg_sys::WARNING as i32,             // log issues and continue startup
        );
    }

    // Most of OpenTelemetry can be disabled with this one neat trick. Propagators must remain
    // active and can be configured in `OTEL_PROPAGATORS`.
    if std::env::var("OTEL_SDK_DISABLED").is_ok_and(|v| v.trim().eq_ignore_ascii_case("true")) {
        set::<4>(c"otel.export_logs", "off");
        set::<4>(c"otel.export_traces", "off");
    } else {
        for (env_name, guc_name) in [
            ("OTEL_TRACES_EXPORTER", c"otel.export_traces"),
            ("OTEL_LOGS_EXPORTER", c"otel.export_logs"),
        ] {
            if let Ok(v) = std::env::var(env_name)
                && let trimmed = v.trim()
                && !trimmed.is_empty()
            {
                match trimmed {
                    s if s.eq_ignore_ascii_case("otlp") => set::<4>(guc_name, "on"),
                    s if s.eq_ignore_ascii_case("none") => set::<4>(guc_name, "off"),
                    _ => {
                        pgrx::ereport!(
                            pgrx::PgLogLevel::WARNING,
                            pgrx::PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                            format!(
                                "unsupported exporter '{trimmed}' in {env_name}; only 'otlp' and 'none' are supported"
                            ),
                        );
                        continue;
                    }
                }
            }
        }

        if let Ok(v) = std::env::var("OTEL_METRICS_EXPORTER")
            && let trimmed = v.trim()
            && !trimmed.is_empty()
            && !trimmed.eq_ignore_ascii_case("none")
        {
            pgrx::ereport!(
                pgrx::PgLogLevel::WARNING,
                pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                format!(
                    "metrics export is not supported by {PG_OTEL_LIBRARY}; ignoring OTEL_METRICS_EXPORTER='{trimmed}'"
                ),
            );
        }

        if let Ok(v) = std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL")
            && let trimmed = v.trim()
            && !trimmed.is_empty()
            && !trimmed.eq_ignore_ascii_case("http/protobuf")
        {
            pgrx::ereport!(
                pgrx::PgLogLevel::WARNING,
                pgrx::PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                format!(
                    "protocol '{trimmed}' is not supported by {PG_OTEL_LIBRARY}; only 'http/protobuf' is supported"
                ),
            );
        }
    }

    let mappings: &[(&str, &ffi::CStr)] = &[
        ("OTEL_SERVICE_NAME", c"otel.service_name"),
        ("OTEL_RESOURCE_ATTRIBUTES", c"otel.resource_attributes"),
        ("OTEL_EXPORTER_OTLP_COMPRESSION", c"otel.otlp_compression"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", c"otel.otlp_endpoint"),
        ("OTEL_EXPORTER_OTLP_HEADERS", c"otel.otlp_headers"),
        ("OTEL_EXPORTER_OTLP_TIMEOUT", c"otel.otlp_timeout"),
    ];

    for &(env_name, guc_name) in mappings {
        if let Ok(v) = std::env::var(env_name)
            && let trimmed = v.trim()
            && !trimmed.is_empty()
        {
            set::<512>(guc_name, trimmed);
        }
    }
}

/// Returns whether export of OpenTelemetry log records is enabled by GUC `otel.export_logs`.
pub fn export_logs() -> bool {
    GUC_EXPORT_LOGS.get()
}

/// Returns whether export of OpenTelemetry trace spans is enabled by GUC `otel.export_traces`.
pub fn export_traces() -> bool {
    GUC_EXPORT_TRACES.get()
}

/// Returns the maximum delay to buffer telemetry items before exporting configured by GUC `otel.otlp_batch_max_delay_ms`.
pub fn otlp_batch_max_delay() -> time::Duration {
    time::Duration::from_millis(GUC_BATCH_MAX_DELAY_MS.get() as u64)
}

/// Returns the maximum number of telemetry records per export batch configured by GUC `otel.otlp_batch_max_items`.
pub fn otlp_batch_max_items() -> usize {
    GUC_BATCH_MAX_ITEMS.get().max(1) as usize
}

/// Returns the parsed base OTLP collector endpoint URL configured by GUC `otel.otlp_endpoint`.
pub fn otlp_endpoint() -> URL {
    Endpoint::from(&GUC_ENDPOINT).unwrap() // already validated
}

/// Returns the parsed custom HTTP headers configured by GUC `otel.otlp_headers`.
pub fn otlp_headers() -> Vec<(String, String)> {
    Headers::from(&GUC_HEADERS)
}

/// Returns the HTTP export request timeout configured by GUC `otel.otlp_timeout_ms`.
pub fn otlp_timeout() -> time::Duration {
    time::Duration::from_millis(GUC_TIMEOUT_MS.get().max(1) as u64)
}

/// Builds the OpenTelemetry [`Resource`] containing `service.name`, `service.version`, and configured attributes.
pub fn resource() -> crate::otlp::Resource {
    use crate::otlp::{AnyValue, AnyValueExt, KeyValue, KeyValueExt, Resource, ResourceExt};

    let mut attrs = resource_attributes();
    let configured_service_name = service_name();

    let final_service_name = match attrs.iter().find(|(k, _)| k == "service.name") {
        Some((_, custom_name)) if configured_service_name == "postgresql" => custom_name.clone(),
        _ => configured_service_name,
    };

    if let Some(existing) = attrs.iter_mut().find(|(k, _)| k == "service.name") {
        existing.1 = final_service_name;
    } else {
        attrs.push(("service.name".to_string(), final_service_name));
    }

    if !attrs.iter().any(|(k, _)| k == "service.version") {
        let version = service_version();
        if !version.is_empty() {
            attrs.push(("service.version".to_string(), version.to_string()));
        }
    }

    let kv_pairs: Vec<KeyValue> = attrs
        .into_iter()
        .map(|(k, v)| KeyValue::new(k, AnyValue::new_string(v)))
        .collect();

    Resource::build().attributes(kv_pairs).finish()
}

/// Returns the parsed resource attributes configured by GUC `otel.resource_attributes`.
pub fn resource_attributes() -> Vec<(String, String)> {
    ResourceAttributes::from(&GUC_RESOURCE_ATTRIBUTES)
}

/// Returns the logical service name configured by GUC `otel.service_name`.
pub fn service_name() -> String {
    GUC_SERVICE_NAME
        .as_cstr()
        .and_then(|c| c.to_str().ok())
        .map(String::from)
        .unwrap_or_else(|| "postgresql".to_string())
}

/// Returns the default service version (PostgreSQL server version).
pub fn service_version() -> &'static str {
    pg_sys::PG_VERSION.to_str().unwrap_or("")
}

/// Returns the maximum ceiling on nested statement spans across a query configured by GUC `otel.max_nested_statements`.
pub fn nested_statements_max() -> u32 {
    GUC_MAX_NESTED_STATEMENTS.get().max(0) as u32
}

/// Returns the requested nested statement span budget for this session configured by GUC `otel.trace_nested_statements`.
pub fn nested_statements_request() -> u32 {
    GUC_TRACE_NESTED_STATEMENTS.get().max(0) as u32
}

/// Returns whether statement execution tracing is enabled by GUC `otel.trace_statements`.
pub fn trace_statements() -> bool {
    GUC_TRACE_STATEMENTS.get()
}

/// Returns whether transaction boundary tracing is enabled by GUC `otel.trace_transactions`.
pub fn trace_transactions() -> bool {
    GUC_TRACE_TRANSACTIONS.get()
}

/// Returns whether parallel worker execution tracing is enabled by GUC `otel.trace_parallel_workers`.
pub fn trace_parallel_workers() -> bool {
    GUC_TRACE_PARALLEL_WORKERS.get()
}

/// Returns whether SQLCommenter comment parsing is enabled by GUC `otel.sqlcommenter`.
pub fn sqlcommenter() -> bool {
    GUC_SQLCOMMENTER.get()
}

/// Clears the session GUCs `otel.traceparent` and `otel.tracestate`.
pub fn clear_trace_context() {
    UPDATING_SELF.set(true);
    GucString::set::<1>(
        c"otel.traceparent",
        "",
        pg_sys::GucContext::PGC_INTERNAL, // part of an internal operation
        pg_sys::GucSource::PGC_S_SESSION, // highest precedence
        pg_sys::PGERROR as i32,
    );
    GucString::set::<1>(
        c"otel.tracestate",
        "",
        pg_sys::GucContext::PGC_INTERNAL, // part of an internal operation
        pg_sys::GucSource::PGC_S_SESSION, // highest precedence
        pg_sys::PGERROR as i32,
    );
    UPDATING_SELF.set(false);
}

/// Reads the incoming W3C trace context from session GUCs `otel.traceparent` and `otel.tracestate` without clearing them.
pub fn read_trace_context() -> Option<crate::context::SpanContext> {
    let tp_str = GUC_TRACEPARENT.as_cstr()?.to_str().ok()?;
    let ts_str = GUC_TRACESTATE.as_cstr().and_then(|s| s.to_str().ok());
    let headers = crate::context::Headers::new(tp_str, ts_str);
    crate::context::SpanContext::try_from(headers).ok()
}

/// Sets session GUCs `otel.traceparent` and `otel.tracestate` to propagate active trace context to parallel workers.
pub fn set_trace_context(traceparent: &str, tracestate: &str) {
    UPDATING_SELF.set(true);
    // W3C traceparent is 55 bytes (§ 3.2) plus some extra room.
    GucString::set::<64>(
        c"otel.traceparent",
        traceparent,
        pg_sys::GucContext::PGC_INTERNAL, // part of an internal operation
        pg_sys::GucSource::PGC_S_SESSION, // highest precedence
        pg_sys::PGERROR as i32,
    );
    if !tracestate.is_empty() {
        // W3C tracestate is recommended to be no more than 512 characters (§ 3.3).
        GucString::set::<512>(
            c"otel.tracestate",
            tracestate,
            pg_sys::GucContext::PGC_INTERNAL, // part of an internal operation
            pg_sys::GucSource::PGC_S_SESSION, // highest precedence
            pg_sys::PGERROR as i32,
        );
    }
    UPDATING_SELF.set(false);
}

/// Consumes and clears incoming W3C trace context from session GUCs `otel.traceparent` and `otel.tracestate`.
pub fn take_trace_context() -> Option<crate::context::SpanContext> {
    let ctx = read_trace_context();
    if ctx.is_some() || GUC_TRACEPARENT.as_cstr().is_some() || GUC_TRACESTATE.as_cstr().is_some() {
        clear_trace_context();
    }
    ctx
}

struct Endpoint;

impl Endpoint {
    pub fn from(trusted: &GucString) -> Option<URL> {
        let s = trusted.as_cstr()?.to_str().ok()?;
        URL::parse(s).ok()
    }

    pub fn validate(untrusted: &ffi::CStr) -> eyre::Result<()> {
        let s = untrusted.to_str()?;
        eyre::ensure!(
            s.starts_with("http://") || s.starts_with("https://"),
            r#"URL must begin with "http" or "https""#,
        );
        URL::parse(s)?;
        Ok(())
    }
}

struct Headers;

impl Headers {
    pub fn from(trusted: &GucString) -> Vec<(String, String)> {
        trusted
            .as_cstr()
            .and_then(|c| c.to_str().ok())
            .map(|s| Self::parse(s).unwrap())
            .unwrap_or_default()
    }

    pub fn validate(untrusted: &ffi::CStr) -> eyre::Result<()> {
        Self::parse(untrusted.to_str()?)?;
        Ok(())
    }

    pub fn parse(raw: &str) -> eyre::Result<Vec<(String, String)>> {
        let mut results = Vec::new();
        if raw.trim().is_empty() {
            return Ok(results);
        }

        // Step 1: Split into raw tokens on unescaped commas
        let mut raw_tokens = Vec::new();
        let mut current = String::new();
        let mut chars = raw.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(&',') = chars.peek() {
                    chars.next();
                    current.push(',');
                    continue;
                }
                current.push('\\');
            } else if c == ',' {
                raw_tokens.push(current);
                current = String::new();
            } else {
                current.push(c);
            }
        }
        if !current.is_empty() {
            raw_tokens.push(current);
        }

        // Step 2: Parse each token into a key-value pair
        for token in raw_tokens {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue; // Skip empty tokens (handling leading/trailing/multiple commas & newlines)
            }

            // Find first unescaped ':' or '='
            let mut delim_pos = None;
            let mut t_chars = trimmed.char_indices().peekable();
            while let Some((idx, ch)) = t_chars.next() {
                if ch == '\\' {
                    t_chars.next(); // Skip escaped character
                } else if ch == ':' || ch == '=' {
                    delim_pos = Some((idx, ch));
                    break;
                }
            }

            let Some((pos, _ch)) = delim_pos else {
                eyre::bail!("invalid header syntax: missing ':' or '=' in '{trimmed}'");
            };

            let key = trimmed[..pos].trim();
            let val = trimmed[pos + 1..].trim();

            if key.is_empty() {
                eyre::bail!("invalid header syntax: key cannot be empty in '{trimmed}'");
            }

            results.push((key.to_string(), val.to_string()));
        }

        Ok(results)
    }
}

struct ResourceAttributes;

impl ResourceAttributes {
    pub fn from(trusted: &GucString) -> Vec<(String, String)> {
        trusted
            .as_cstr()
            .and_then(|c| c.to_str().ok())
            .map(|s| Self::parse(s).unwrap_or_default())
            .unwrap_or_default()
    }

    pub fn validate(untrusted: &ffi::CStr) -> eyre::Result<()> {
        Self::parse(untrusted.to_str()?)?;
        Ok(())
    }

    pub fn parse(raw: &str) -> eyre::Result<Vec<(String, String)>> {
        let mut results = Vec::new();
        if raw.trim().is_empty() {
            return Ok(results);
        }

        for token in raw.split(',') {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }

            let Some((k_raw, v_raw)) = trimmed.split_once('=') else {
                eyre::bail!("invalid resource attribute syntax: missing '=' in '{trimmed}'");
            };

            let k = crate::encoding::percent_decode(k_raw.trim())?;
            if k.is_empty() {
                eyre::bail!(
                    "invalid resource attribute syntax: key cannot be empty in '{trimmed}'"
                );
            }

            let v = crate::encoding::percent_decode(v_raw.trim())?;

            let k_string = k.into_owned();
            let v_string = v.into_owned();

            if let Some(existing) = results.iter_mut().find(|(k, _)| k == &k_string) {
                existing.1 = v_string;
            } else {
                results.push((k_string, v_string));
            }
        }

        Ok(results)
    }
}

/// Invokes a closure with a temporary, null-terminated [`ffi::CStr`] reference, avoiding
/// allocations when the string fits in stack buffer `N`.
///
/// Returns `None` when `s` contains null bytes.
fn with_stack_cstr<const N: usize, R>(s: &str, f: impl FnOnce(&ffi::CStr) -> R) -> Option<R> {
    // Fallback to CString (allocation) when the value doesn't fit within the expected size.
    // When the value contains a null, let CString assess and return None.
    if s.len() < N && !s.as_bytes().contains(&0) {
        let mut buf = [0u8; N];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        let cstr = ffi::CStr::from_bytes_with_nul(&buf[..=s.len()]).ok()?;
        Some(f(cstr))
    } else {
        let cstr = ffi::CString::new(s).ok()?;
        Some(f(&cstr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[test]
    fn headers_parse_basic() {
        let input = "X-API-Key: secret123, env=production";
        assert_that!(
            Headers::parse(input),
            ok(unordered_elements_are![
                (eq("X-API-Key"), eq("secret123")),
                (eq("env"), eq("production")),
            ])
        );
    }

    #[test]
    fn headers_parse_first_delimiter_split() {
        let input = "X-Callback: https://example.com/api?a=1, Auth=token==";
        assert_that!(
            Headers::parse(input),
            ok(unordered_elements_are![
                (eq("X-Callback"), eq("https://example.com/api?a=1")),
                (eq("Auth"), eq("token==")),
            ])
        );
    }

    #[test]
    fn headers_parse_multiline_and_extra_commas() {
        let input = "
            , X-Scope-OrgID: tenant_101 ,

            Authorization: Bearer my_secret_token ,
            ,
        ";
        assert_that!(
            Headers::parse(input),
            ok(unordered_elements_are![
                (eq("X-Scope-OrgID"), eq("tenant_101")),
                (eq("Authorization"), eq("Bearer my_secret_token")),
            ])
        );
    }

    #[test]
    fn headers_parse_escaped_commas() {
        let input = r"Authorization: Bearer foo\,bar, env: prod";
        assert_that!(
            Headers::parse(input),
            ok(unordered_elements_are![
                (eq("Authorization"), eq("Bearer foo,bar")),
                (eq("env"), eq("prod")),
            ])
        );
    }

    #[test]
    fn headers_parse_invalid_syntax_error() {
        let input = "invalid_header_without_delimiter";
        assert_that!(
            Headers::parse(input),
            err(displays_as(contains_substring(
                "invalid header syntax: missing ':' or '='"
            )))
        );

        let input_empty_key = ": value_without_key";
        assert_that!(
            Headers::parse(input_empty_key),
            err(displays_as(contains_substring(
                "invalid header syntax: key cannot be empty"
            )))
        );
    }

    #[test]
    fn stack_cstr_handling() {
        // Stack path (< N)
        let res = with_stack_cstr::<16, _>("hello", |c| c.to_str().unwrap().to_string());
        assert_that!(res, some(eq("hello")));

        // Fallback heap path (>= N)
        let res = with_stack_cstr::<4, _>("toolong", |c| c.to_str().unwrap().to_string());
        assert_that!(res, some(eq("toolong")));

        // Reject interior nul byte
        let res = with_stack_cstr::<16, _>("bad\0str", |_| ());
        assert_that!(res, none());
    }

    #[test]
    fn resource_attributes_parse_basic() {
        let input = "service.name=my-service,deployment.environment=production";
        assert_that!(
            ResourceAttributes::parse(input),
            ok(unordered_elements_are![
                (eq("service.name"), eq("my-service")),
                (eq("deployment.environment"), eq("production")),
            ])
        );
    }

    #[test]
    fn resource_attributes_parse_percent_encoded() {
        let input = "service.name=my%20service,tags=foo%2Cbar%3Dbaz,unicode=%C3%BCber";
        assert_that!(
            ResourceAttributes::parse(input),
            ok(unordered_elements_are![
                (eq("service.name"), eq("my service")),
                (eq("tags"), eq("foo,bar=baz")),
                (eq("unicode"), eq("über")),
            ])
        );
    }

    #[test]
    fn resource_attributes_parse_whitespace_and_empty() {
        let input = "  k1 = v1 , , k2 = v2 , ";
        assert_that!(
            ResourceAttributes::parse(input),
            ok(unordered_elements_are![
                (eq("k1"), eq("v1")),
                (eq("k2"), eq("v2")),
            ])
        );

        assert_that!(ResourceAttributes::parse(""), ok(is_empty()));
        assert_that!(ResourceAttributes::parse("   "), ok(is_empty()));
    }

    #[test]
    fn resource_attributes_duplicate_keys() {
        let input = "k1=first,k2=foo,k1=second";
        assert_that!(
            ResourceAttributes::parse(input),
            ok(unordered_elements_are![
                (eq("k1"), eq("second")),
                (eq("k2"), eq("foo")),
            ])
        );
    }

    #[test]
    fn resource_attributes_parse_invalid_syntax() {
        let input = "missing_delimiter";
        assert_that!(
            ResourceAttributes::parse(input),
            err(displays_as(contains_substring(
                "invalid resource attribute syntax: missing '='"
            )))
        );

        let input_empty_key = "=value";
        assert_that!(
            ResourceAttributes::parse(input_empty_key),
            err(displays_as(contains_substring(
                "invalid resource attribute syntax: key cannot be empty"
            )))
        );

        let input_invalid_hex = "k=%ZZ";
        assert_that!(
            ResourceAttributes::parse(input_invalid_hex),
            err(displays_as(contains_substring("is not valid hex")))
        );

        let input_truncated = "k=%2";
        assert_that!(
            ResourceAttributes::parse(input_truncated),
            err(displays_as(contains_substring("truncated sequence")))
        );
    }

    #[test]
    fn resource_default_construction() {
        use crate::otlp::{AnyValue, AnyValueExt, KeyValue, KeyValueExt};

        let res = resource();
        assert_that!(
            res.attributes,
            contains(eq(&KeyValue::new(
                "service.name",
                AnyValue::new_string("postgresql")
            )))
        );
        assert_that!(
            res.attributes,
            contains(eq(&KeyValue::new(
                "service.version",
                AnyValue::new_string(service_version())
            )))
        );
    }
}
