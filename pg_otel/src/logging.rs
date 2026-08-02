// SPDX-License-Identifier: MIT

use crate::PG_OTEL_LIBRARY;
use pgrx::pg_sys;
use std::{ffi, time};

pub fn install_hooks() {
    debug_assert!(crate::assert_postmaster_startup());

    /// All logging in Postgres is done through the `ereport` macro that
    /// (1) populates an `ErrorData` struct,
    /// (2) passes it to `emit_log_hook`, then
    /// (3) sends it to the logging collector or stderr.
    ///
    /// NOTE: The hook is **not** called when the `output_to_server` field is false.
    /// NOTE: The hook may **not** change the contents of `ErrorData` **except** to change `output_to_server` to false.
    ///
    /// - https://doxygen.postgresql.org/structErrorData.html
    /// - https://doxygen.postgresql.org/elog_8c.html
    ///
    /// # Safety
    ///
    /// This variable is assigned during Postmaster startup and inherited by every child process.
    static mut HOOK_EMIT_LOG: pg_sys::emit_log_hook_type = None;
    unsafe {
        HOOK_EMIT_LOG = pg_sys::emit_log_hook;
        pg_sys::emit_log_hook = Some(emit_log_hook);
    }

    /// Called by Postgres when a log message is not suppressed by GUC `log_min_messages`.
    ///
    /// - https://www.postgresql.org/docs/current/runtime-config-logging.html#GUC-LOG-MIN-MESSAGES
    ///
    /// # Safety
    ///
    /// This is called by Postgres (from C into Rust) and *MUST* have the [`pgrx::pg_guard`] attribute.
    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn emit_log_hook(edata: *mut pg_sys::ErrorData) {
        use pgrx::bgworkers::BackgroundWorker;

        // SAFETY: It is safe to read this variable because this hook is called by Postgres.
        let worker = unsafe { !pg_sys::MyBgworkerEntry.is_null() };
        let exporter = worker && BackgroundWorker::get_extra() == "E";

        let log_exports_enabled = true;

        // Export log messages when configured to do so. Sending messages *from* the exporter *to* the
        // exporter could cause a feedback loop, so don't do that. These messages still go to the next
        // log processor which is usually Postgres' built-in logging collector or stderr.
        if exporter || !log_exports_enabled {
            return call_remaining_hooks(edata);
        }

        // Get the current time before calling other hooks.
        let now = time::SystemTime::now();

        // Call other hooks so they can manipulate edata before we record it.
        call_remaining_hooks(edata);

        // Gather context and send the log message to the background worker.
        // When logging breaks down, print to STDERR as a last resort.
        if edata.is_aligned()
            && let Some(record) = unsafe { edata.as_ref() }
            && record.output_to_server
            && let Err(error) = export_log_record(&now, &record)
        {
            eprintln!("{PG_OTEL_LIBRARY}: unable to move log internally: {error}");
        }

        // SAFETY: HOOK_EMIT_LOG is assigned above, during startup; it is safe to read here.
        // SAFETY: pg_guard_ffi_boundary handles any Postgres error that occurs inside the next hook.
        fn call_remaining_hooks(edata: *mut pg_sys::ErrorData) {
            if let Some(next) = unsafe { HOOK_EMIT_LOG } {
                unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(edata)) };
            }
        }
    }
}

/// Combines timestamp, edata, and Postgres process metadata into an OTLP LogsData payload
/// and pushes it into the shared memory queue.
fn export_log_record(timestamp: &time::SystemTime, edata: &pg_sys::ErrorData) -> eyre::Result<()> {
    use crate::otlp::*;

    // TODO: let encoding = unsafe { pg_sys::GetMessageEncoding() };

    let mut attributes = KeyValueListBuilder::with_capacity(15);
    attributes.int("process.pid", unsafe { pg_sys::MyProcPid });

    // SAFETY: MyProcPort is safe to read here, and as_ref checks for null.
    if let Some(port) = unsafe { pg_sys::libpq::be::MyProcPort.as_ref() } {
        if !port.remote_host.is_null() {
            attributes.str_unchecked("client.address", port.remote_host);
        }
        if !port.remote_port.is_null()
            && let s = unsafe { ffi::CStr::from_ptr(port.remote_port) }
            && let Some(i) = atoi::atoi::<i64>(s.to_bytes())
        {
            attributes.int("client.port", i);
        }
        if !port.database_name.is_null() {
            attributes.str_unchecked("db.namespace", port.database_name);
        }
        if !port.user_name.is_null() {
            attributes.str_unchecked("db.postgresql.user", port.user_name);
        }
    }

    if !edata.filename.is_null() {
        attributes.str_unchecked("code.filepath", edata.filename);
        attributes.int("code.lineno", edata.lineno);
    }

    if !edata.funcname.is_null() {
        attributes.str_unchecked("code.function", edata.funcname);
    }

    let name = unsafe { pg_sys::application_name };
    if !name.is_null() && unsafe { *name } != 0 {
        attributes.str_unchecked("db.postgresql.application_name", name);
    }

    if !edata.context.is_null() && !edata.hide_ctx {
        attributes.str_unchecked("db.postgresql.context", edata.context);
    }

    if !edata.detail_log.is_null() {
        attributes.str_unchecked("db.postgresql.detail", edata.detail_log);
    } else if !edata.detail.is_null() {
        attributes.str_unchecked("db.postgresql.detail", edata.detail);
    }

    if !edata.hint.is_null() {
        attributes.str_unchecked("db.postgresql.hint", edata.hint);
    }

    if edata.sqlerrcode != 0 {
        attributes.str_unchecked("db.postgresql.state_code", unsafe {
            pg_sys::unpack_sql_state(edata.sqlerrcode)
        });
    }

    // Select severity number and text according to the OpenTelemetry Log Data Model and the
    // Postgres error_severity function.
    //
    // https://doxygen.postgresql.org/elog_8c.html
    // https://opentelemetry.io/docs/specs/otel/logs/data-model
    // https://www.postgresql.org/docs/current/runtime-config-logging.html#RUNTIME-CONFIG-SEVERITY-LEVELS
    //
    // > ["SeverityText"] is the original string representation of the severity as it is known at the source.
    //
    // > If "SeverityNumber" is present and has a value of ERROR (numeric 17) or higher
    // > then it is an indication that the log record represents an erroneous situation.
    //
    // > If the log record represents a non-erroneous event the "SeverityNumber" field …
    // > may be set to any numeric value less than ERROR (numeric 17).
    //
    // > Smaller numerical values correspond to less severe events (such as debug events),
    // > larger numerical values correspond to more severe events (such as errors and critical events).
    //
    // > If the source format has only a single severity that matches the meaning of the range
    // > then it is recommended to assign that severity the smallest value of the range.
    //
    let severity = match edata.elevel as u32 {
        pg_sys::PGERROR => Some(("ERROR", LogSeverity::Error /*  17 */)), // more severe
        pg_sys::WARNING => Some(("WARNING", LogSeverity::Warn /* 13 */)), // less severe

        // Postgres LOG and LOG_SERVER_ONLY are conceptually the same severity.
        // They differ in that clients may opt-in to receiving LOG messages,
        // while LOG_SERVER_ONLY never goes to clients.
        pg_sys::LOG | pg_sys::LOG_SERVER_ONLY => Some(("LOG", LogSeverity::Info)),

        // OTel does not have a NOTICE range, so mapping it is a little tricky.
        // When Postgres sends its log messages directly to syslog or Windows eventlog:
        //
        //  - Postgres INFO maps to syslog `info` and eventlog `INFORMATION`
        //  - Postgres NOTICE maps to syslog `notice` and eventlog `INFORMATION`
        //
        // These levels are all "normal" or "informational" but `notice` is more severe in syslog.
        // OTel recommends mapping syslog `notice` to OTel INFO2, and that aligns with all the above.
        //
        // [syslog]: https://www.rfc-editor.org/info/rfc5424
        pg_sys::INFO => Some(("INFO", LogSeverity::Info /*       9 */)), // less severe
        pg_sys::NOTICE => Some(("NOTICE", LogSeverity::Info2 /* 10 */)), // more severe

        // Postgres numbers DEBUG1 through DEBUG5 as least verbose to most verbose,
        // which is effectively most severe to least severe.
        //
        // OTel has only four options in its DEBUG range, so we have chosen to map
        // Postgres DEBUG1 to OTel DEBUG and the others to the OTel TRACE range.
        //
        // NOTE: Postgres DEBUG1 is the level used by GUC debug_* variables.
        pg_sys::DEBUG5 => Some(("DEBUG", LogSeverity::Trace /*  1 */)), // least severe
        pg_sys::DEBUG4 => Some(("DEBUG", LogSeverity::Trace2 /* 2 */)), //
        pg_sys::DEBUG3 => Some(("DEBUG", LogSeverity::Trace3 /* 3 */)), //
        pg_sys::DEBUG2 => Some(("DEBUG", LogSeverity::Trace4 /* 4 */)), //
        pg_sys::DEBUG1 => Some(("DEBUG", LogSeverity::Debug /*  5 */)), //

        // FATAL messages implicitly abort the current transaction, and
        // PANIC messages implicitly abort all database sessions.
        pg_sys::FATAL => Some(("FATAL", LogSeverity::Fatal /*  21 */)), // less severe
        pg_sys::PANIC => Some(("PANIC", LogSeverity::Fatal2 /* 22 */)), // more severe

        // WARNING_CLIENT_ONLY is available since Postgres 14.
        // The log hook is not called for this severity, but it is included here for completeness.
        #[cfg(not(feature = "pg13"))]
        pg_sys::WARNING_CLIENT_ONLY => crate::unlikely(Some(("WARNING", LogSeverity::Warn))),

        // FATAL_CLIENT_ONLY is available since Postgres 19.
        // The log hook is not called for this severity, but it is included here for completeness.
        #[cfg(feature = "pg19")]
        pg_sys::FATAL_CLIENT_ONLY => crate::unlikely(Some(("FATAL", LogSeverity::Fatal))),

        _ => crate::unlikely(None),
    };

    let mut record = LogRecord::build()
        .attributes(attributes.finish().values)
        .body(new_str_unchecked(edata.message))
        .time_unix_nano(
            timestamp
                .duration_since(time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
        );

    if let Some((t, n)) = severity {
        record = record.severity_number(n).severity_text(t)
    }

    let mut buffer = crate::BytesMut::new();
    prost::Message::encode(&record.finish(), &mut buffer)?;
    Ok(crate::exporter::send_one(&buffer))
}

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use crate::otlp::*;
    use googletest::prelude::*;
    use prost::Message;
    use std::{thread, time};

    #[pgrx::pg_test]
    fn e2e_otlp_export() {
        crate::acquire_test_lock();

        // setup: clear the mock collector
        let endpoint = format!("{}/test/logs", crate::exporter::endpoint());
        reqwest::blocking::get(&endpoint).unwrap();

        pgrx::warning!("integration test log record message");

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        // retrieve records from the collector with timeout
        while !done && start.elapsed() < time::Duration::from_secs(1) {
            thread::sleep(time::Duration::from_millis(50));

            let body = reqwest::blocking::get(&endpoint).unwrap().bytes().unwrap();
            done = body.len() == 0 && data.len() > 0;
            data.extend(LogsData::decode(body).unwrap().resource_logs);
        }

        assert_that!(
            data,
            contains(matches_pattern!(crate::otlp::ResourceLogs {
                scope_logs: contains(matches_pattern!(crate::otlp::ScopeLogs {
                    log_records: contains(matches_pattern!(crate::otlp::LogRecord {
                        attributes: contains(eq(&crate::otlp::KeyValue::new(
                            "process.pid",
                            AnyValue::new_int(unsafe { pgrx::pg_sys::MyProcPid }),
                        ))),
                        body: some(eq(&AnyValue::new_string(
                            "integration test log record message"
                        ))),
                        severity_text: eq("WARNING"),
                        ..
                    })),
                    ..
                })),
                ..
            })),
        );
    }
}
