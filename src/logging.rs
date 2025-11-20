// SPDX-License-Identifier: ISC

use pgrx::{PgBox, pg_sys};
use std::{ffi, time};

const ENCODING: bincode::config::Configuration = bincode::config::standard();

/// IPC does only simple routing of `Vec<u8>` values.
#[derive(bincode::BorrowDecode, bincode::Encode)]
enum Message<'a> {
    LogRecord(DecodedLogRecord<'a>),
}

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
        use crate::ipc::SyncSender;
        use pgrx::bgworkers::BackgroundWorker;

        // SAFETY: It is safe to read this variable because this hook is called by Postgres.
        let worker = unsafe { !pg_sys::MyBgworkerEntry.is_null() };
        let exporter = worker && BackgroundWorker::get_extra() == "E";

        // Export log messages when configured to do so. Sending messages *from* the exporter *to* the
        // exporter could cause a feedback loop, so don't do that. These messages still go to the next
        // log processor which is usually Postgres' built-in logging collector or stderr.
        if exporter || !crate::config::exporting(crate::config::Logs) {
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
            && let Err(error) = || -> Result<(), crate::ipc::Error> {
                crate::TO_EXPORTER
                    .wait()
                    .send(crate::ipc::Message::Logs(DecodedLogRecord::serialize(&now, &record)?))
            }()
        {
            eprintln!("{}: unable to move log internally: {error}", crate::PG_OTEL_LIBRARY);
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

/// DecodedLogRecord is a short-lived reference to information owned elsewhere that can be
/// serialized, sent, and received over IPC.
///
/// # Design
///
/// TODO: better explain:
/// - cheap to encode from Postgres runtime values
/// - the receiver does parsing and error handling
///
#[derive(bincode::BorrowDecode, bincode::Encode)]
struct DecodedLogRecord<'a> {
    timestamp_unix_nano: u64,
    pid: ffi::c_int,
    level: i32,
    body: &'a [u8],
    encoding: ffi::c_int,

    application: Option<&'a [u8]>,
    code: Option<(&'a [u8], i32)>,
    context: Option<&'a [u8]>,
    database_name: Option<&'a [u8]>,
    detail: Option<&'a [u8]>,
    func_name: Option<&'a [u8]>,
    hint: Option<&'a [u8]>,
    sql_state: Option<&'a [u8]>,
    user_name: Option<&'a [u8]>,
}

impl DecodedLogRecord<'_> {
    /// This combines timestamp, edata, and information about the current Postgres process into a
    /// serialized [`Message::LogRecord`]. It must be called from within the Postgres main thread.
    fn serialize(
        timestamp: &time::SystemTime,
        edata: &PgBox<pg_sys::ErrorData>,
    ) -> Result<Vec<u8>, crate::ipc::Error> {
        let r = Self {
            timestamp_unix_nano: timestamp
                .duration_since(time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,

            pid: unsafe { pg_sys::MyProcPid },
            body: unsafe { ffi::CStr::from_ptr(edata.message) }.to_bytes(),
            level: edata.elevel,

            application: Some(unsafe { pg_sys::application_name })
                .filter(|p| !p.is_null())
                .map(|p| unsafe { ffi::CStr::from_ptr(p) })
                .filter(|s| !s.is_empty())
                .map(|s| s.to_bytes()),

            code: Some((edata.filename, edata.lineno))
                .filter(|(p, _)| !p.is_null())
                .map(|(p, n)| (unsafe { ffi::CStr::from_ptr(p) }.to_bytes(), n)),

            context: Some(edata.context)
                .filter(|p| !p.is_null() && !edata.hide_ctx)
                .map(|p| unsafe { ffi::CStr::from_ptr(p) }.to_bytes()),

            database_name: None,

            detail: Some(edata.detail_log)
                .filter(|p| !p.is_null())
                .or(Some(edata.detail))
                .filter(|p| !p.is_null())
                .map(|p| unsafe { ffi::CStr::from_ptr(p) }.to_bytes()),

            encoding: unsafe { pg_sys::GetMessageEncoding() },

            func_name: Some(edata.funcname)
                .filter(|p| !p.is_null())
                .map(|p| unsafe { ffi::CStr::from_ptr(p) }.to_bytes()),

            hint: Some(edata.hint)
                .filter(|p| !p.is_null())
                .map(|p| unsafe { ffi::CStr::from_ptr(p) }.to_bytes()),

            // Humans expect a 5-byte SQLSTATE string, but it is encoded here as a 4-byte integer.
            //
            // - https://en.wikipedia.org/wiki/SQLSTATE
            //
            // The unpack_sql_state function decodes it but cannot be called at the receiver because
            // (1) C functions must be called by the main thread and
            // (2) it returns a pointer to a static buffer making it absolutely *not* thread-safe.
            //
            // Decode it now, during this hook, which Postgres calls on the main thread.
            sql_state: Some(edata.sqlerrcode)
                .filter(|n| *n != 0)
                .map(|n| unsafe { pg_sys::unpack_sql_state(n) })
                .map(|p| unsafe { ffi::CStr::from_ptr(p) }.to_bytes()),

            user_name: None,
        };

        //if let Some(port) = pg_sys::MyProcPort.as_ref() {}

        Ok(bincode::encode_to_vec(Message::LogRecord(r), ENCODING)?)
    }
}
