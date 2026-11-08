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

use core::cell::Cell;
use pgrx::pg_sys;
use std::{ffi, time};
use url::Url as URL;

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
static GUC_HEADERS: GucString = GucString::new(None);
static GUC_MAX_NESTED_STATEMENTS: GucInt32 = GucInt32::new(50);
static GUC_PROTOCOL: GucString = GucString::new(Some(c"http/protobuf"));
static GUC_SIGNALS_ENABLED: GucString = GucString::new(None);
static GUC_TIMEOUT_MS: GucInt32 = GucInt32::new(10000);
static GUC_TRACE_NESTED_STATEMENTS: GucInt32 = GucInt32::new(0);
static GUC_TRACEPARENT: GucString = GucString::new(None);
static GUC_TRACESTATE: GucString = GucString::new(None);

thread_local! {
    static EXPORT_LOGS: Cell<bool> = const { Cell::new(false) };
    static EXPORT_SPANS: Cell<bool> = const { Cell::new(false) };
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

    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.export",
            c"Telemetry signals to export",
            c"Comma-separated list of signals to export (traces, logs).",
            &GUC_SIGNALS_ENABLED,
            GucContext::Userset, // TODO
            GucFlags::empty()
                | GucFlags::from_bits_retain(pg_sys::GUC_IS_NAME as i32)
                | GucFlags::from_bits_retain(pg_sys::GUC_LIST_INPUT as i32)
                | GucFlags::from_bits_retain(pg_sys::GUC_NOT_WHILE_SEC_REST as i32),
            Some(check_signals),
            Some(assign_signals),
            None,
        );

        #[pgrx::pg_guc_hook(check)]
        fn check_signals(value: Option<ffi::CString>) -> Result<(), GucCheckError> {
            if let Some(value) = value.as_deref() {
                value
                    .to_str()
                    .map_err(eyre::Report::from)
                    .and_then(Signals::parse)
                    .map_err(check_error)?;
            }
            Ok(())
        }

        #[pgrx::pg_guc_hook(assign)]
        fn assign_signals(value: Option<ffi::CString>) {
            let (logs, spans) = value
                .map(|v| Signals::parse(&v.to_string_lossy()).unwrap())
                .unwrap_or((false, false));

            EXPORT_LOGS.set(logs);
            EXPORT_SPANS.set(spans);
        }
    }

    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.otlp_endpoint",
            c"Base OTLP collector endpoint URL",
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
            c"OTLP headers for all signals (superuser only)",
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
        c"Exporter protocol (http/protobuf or http/json).",
        &GUC_PROTOCOL,
        GucContext::Sighup, // config file, affects entire server, requires reload
        GucFlags::empty(),  // visible to everyone
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
        c"Send timeout",
        c"Request timeout duration in milliseconds.",
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

    GucRegistry::define_int_guc(
        c"otel.max_nested_statements",
        c"Maximum nested statement span limit",
        c"Maximum number of nested statement spans that may be emitted across a statement.",
        &GUC_MAX_NESTED_STATEMENTS,
        0,
        i32::MAX / 4,
        GucContext::Suset, // superuser can set for session, user, database
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
                    if crate::tracing::execution_depth() > 1 {
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
                if crate::tracing::execution_depth() > 1 {
                    return Err(check_error("cannot set inside a nested statement"));
                }
            }
            Ok(())
        }
    }
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

/// Returns whether export of OpenTelemetry log records is enabled by GUC `otel.export`.
pub fn logs_enabled() -> bool {
    EXPORT_LOGS.get()
}

/// Returns whether export of OpenTelemetry trace spans is enabled by GUC `otel.export`.
pub fn spans_enabled() -> bool {
    EXPORT_SPANS.get()
}

/// Returns the maximum ceiling on nested statement spans across a query configured by GUC `otel.max_nested_statements`.
pub fn nested_statements_max() -> u32 {
    GUC_MAX_NESTED_STATEMENTS.get().max(0) as u32
}

/// Returns the requested nested statement span budget for this session configured by GUC `otel.trace_nested_statements`.
pub fn nested_statements_request() -> u32 {
    GUC_TRACE_NESTED_STATEMENTS.get().max(0) as u32
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

struct Signals;

impl Signals {
    /// Parses a comma-separated list of signals (e.g. `logs,spans`) into `(logs, spans)` booleans.
    pub fn parse(raw: &str) -> eyre::Result<(bool, bool)> {
        let (mut logs, mut spans) = (false, false);
        for part in raw.split(',') {
            match part.trim() {
                "" => (),
                "log" | "logs" => logs = true,
                "span" | "spans" | "trace" | "traces" => spans = true,
                v => eyre::bail!("unrecognized signal: {:?}", v),
            }
        }
        Ok((logs, spans))
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
    fn signals() {
        assert_that!(Signals::parse(""), ok(eq(&(false, false))));
        assert_that!(Signals::parse(",, "), ok(eq(&(false, false))));

        assert_that!(Signals::parse("log"), ok(eq(&(true, false))));
        assert_that!(Signals::parse("logs"), ok(eq(&(true, false))));

        assert_that!(Signals::parse("span"), ok(eq(&(false, true))));
        assert_that!(Signals::parse("traces"), ok(eq(&(false, true))));

        assert_that!(Signals::parse("trace,log"), ok(eq(&(true, true))));
        assert_that!(Signals::parse("logs, span"), ok(eq(&(true, true))));

        assert_that!(Signals::parse("all"), err(anything()));
        assert_that!(Signals::parse("none"), err(anything()));
        assert_that!(Signals::parse("logs,none"), err(anything()));
        assert_that!(Signals::parse("off"), err(anything()));
        assert_that!(Signals::parse("invalid"), err(anything()));
    }

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
}
