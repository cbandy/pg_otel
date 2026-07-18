// SPDX-License-Identifier: MIT

use crate::{PG_OTEL_LIBRARY, PG_OTEL_VERSION};
use pgrx::{PgBox, pg_sys};
use std::{convert, time};

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

        let log_exports_enabled = false;

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
        //
        // SAFETY: The edata value is owned by Postgres, usually in ErrorContext.
        let record = unsafe { PgBox::from_pg(edata) };
        if !record.is_null()
            && record.output_to_server
            && let Err(error) = || -> eyre::Result<()> {
                let _msg: TODO = (&now, &record).try_into()?;
                Ok(())
            }()
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

struct TODO(String);

impl convert::TryFrom<(&time::SystemTime, &PgBox<pg_sys::ErrorData>)> for TODO {
    type Error = eyre::Error;

    /// This combines timestamp, edata, and information about the current Postgres process.
    /// It must be called from within the Postgres main thread.
    fn try_from(args: (&time::SystemTime, &PgBox<pg_sys::ErrorData>)) -> eyre::Result<Self> {
        use crate::otlp::*;

        let (timestamp, edata) = args;
        let timestamp = timestamp
            .duration_since(time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        // TODO: let encoding = unsafe { pg_sys::GetMessageEncoding() };
        // TODO: if let Some(port) = pg_sys::MyProcPort.as_ref() {}

        let mut attributes = KeyValueListBuilder::with_capacity(10);
        attributes.int("process.pid", unsafe { pg_sys::MyProcPid });

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

        // Humans expect a 5-byte SQLSTATE string, but it is encoded here as a 4-byte integer.
        //
        // - https://en.wikipedia.org/wiki/SQLSTATE
        //
        // The unpack_sql_state function decodes it but cannot be called at the receiver because
        // (1) C functions must be called by the main thread and
        // (2) it returns a pointer to a static buffer making it absolutely *not* thread-safe.
        //
        // Decode it now, during this hook, which Postgres calls on the main thread.
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
            .time_unix_nano(timestamp);

        if let Some((t, n)) = severity {
            record = record.severity_number(n).severity_text(t)
        }

        let _data = LogsData::new([ResourceLogs::new(
            Resource::build().finish(),
            [ScopeLogs::new(
                InstrumentationScope::build()
                    .name(PG_OTEL_LIBRARY)
                    .version(PG_OTEL_VERSION)
                    .finish(),
                [record.finish()],
            )],
        )]);

        let mut _buffer = crate::BytesMut::new();
        // TODO: protobuf encode(&data, &mut buffer)?

        Ok(TODO("".to_owned()))
    }
}
