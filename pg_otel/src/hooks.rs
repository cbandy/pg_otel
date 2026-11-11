// SPDX-License-Identifier: MIT

//! Interception layer for PostgreSQL backend hooks and callbacks.
//!
//! # Architecture & Responsibilities
//!
//! PostgreSQL exposes extension hook points via global function pointers and callback registries.
//! Extensions must cooperate: each hook point is intercepted at most once per process and must call
//! any previously registered hook.
//!
//! This module is intended to manage all the hooks for this crate. A single hook function is
//! registered for each PostgreSQL lifecycle event and calls other modules as needed.
//!
//! # FFI, Safety, and Boundary Protection
//!
//! - **Entry Point Guards**: Every hook is called directly by PostgreSQL through the C ABI
//!   `extern "C-unwind"` and must be annotated with `#[pgrx::pg_guard]` to ensure Rust panics don't
//!   unwind across C ABI frames.
//! - **Chaining Boundaries**: When chaining to downstream C hooks, calls must be wrapped in
//!   `pg_sys::ffi::pg_guard_ffi_boundary` to interact correctly with PostgreSQL error recovery.
//!
//! # Aliasing Rules Across Postgres Calls
//!
//! Hook arguments arrive as raw C pointers. Downstream hooks and PostgreSQL internals are C code
//! that may inspect or mutate these structures. Holding an active immutable Rust reference across
//! a call into those functions violates Rust aliasing rules. Therefore, all pre-chain reads must
//! end before invoking downstream hooks, and post-chain reads must re-borrow from the raw pointer
//! to observe any mutations made downstream.

use crate::PG_OTEL_LIBRARY;
use pgrx::bgworkers::BackgroundWorker;
use pgrx::pg_sys;
use std::{ffi, ptr, time};

#[allow(clippy::missing_safety_doc)]
pub fn install() {
    debug_assert!(crate::assert_postmaster_startup());

    /// All logging in Postgres is initiated via the `elog` and `ereport` macros and processed by
    /// `EmitErrorReport`, which:
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

    /// Called by Postgres when a log message is not suppressed by parameter
    /// [`log_min_messages`](https://www.postgresql.org/docs/current/runtime-config-logging.html#GUC-LOG-MIN-MESSAGES).
    ///
    /// # Safety
    ///
    /// This is called by Postgres via C FFI and must have the `#[pgrx::pg_guard]` attribute.
    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn emit_log_hook(edata: *mut pg_sys::ErrorData) {
        let call_remaining_hooks = || unsafe {
            if let Some(next) = HOOK_EMIT_LOG {
                pg_sys::ffi::pg_guard_ffi_boundary(|| next(edata));
            }
        };

        let is_worker = unsafe { !pg_sys::MyBgworkerEntry.is_null() };
        let is_exporter = is_worker && BackgroundWorker::get_extra() == "E";

        if is_exporter {
            call_remaining_hooks();
            return;
        }

        if crate::config::export_traces()
            && let Some(record) = unsafe { edata.as_ref() }
        {
            crate::tracing::emit_log(record);
        }

        if crate::config::export_logs() {
            // Get the current time before calling other hooks.
            let now = time::SystemTime::now();

            // Call other hooks so they can manipulate edata before we record it.
            call_remaining_hooks();

            if let Some(record) = unsafe { edata.as_ref() }
                && let Err(error) = crate::logging::emit_log(now, record)
            {
                eprintln!("{PG_OTEL_LIBRARY}: unable to move log internally: {error}");
            }
            return;
        }

        call_remaining_hooks();
    }

    static mut HOOK_PROCESS_UTILITY: pg_sys::ProcessUtility_hook_type = None;
    unsafe {
        HOOK_PROCESS_UTILITY = pg_sys::ProcessUtility_hook;
        pg_sys::ProcessUtility_hook = Some(process_utility_hook);
    }

    /// Intercepts utility statements in PostgreSQL.
    ///
    /// # Safety
    ///
    /// This is called by Postgres via C FFI and must have the `#[pgrx::pg_guard]` attribute.
    #[pgrx::pg_guard]
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C-unwind" fn process_utility_hook(
        ps: *mut pg_sys::PlannedStmt,
        qs: *const ffi::c_char,
        ro: bool,
        pc: pg_sys::ProcessUtilityContext::Type,
        pl: pg_sys::ParamListInfo,
        qe: *mut pg_sys::QueryEnvironment,
        dr: *mut pg_sys::DestReceiver,
        qc: *mut pg_sys::QueryCompletion,
    ) {
        let call_remaining_hooks = || unsafe {
            if let Some(next) = HOOK_PROCESS_UTILITY {
                pg_sys::ffi::pg_guard_ffi_boundary(|| next(ps, qs, ro, pc, pl, qe, dr, qc));
            } else {
                pg_sys::standard_ProcessUtility(ps, qs, ro, pc, pl, qe, dr, qc);
            }
        };

        if crate::config::export_traces()
            && let Some(pstmt) = unsafe { ps.as_ref() }
            && let Some(tree) = unsafe { pstmt.utilityStmt.as_ref() }
        {
            // This pointer is always populated, but it might not decode as UTF-8.
            let sql = (!qs.is_null())
                .then(|| unsafe { ffi::CStr::from_ptr(qs).to_str().ok() })
                .flatten();

            crate::tracing::process_utility(pstmt, tree, sql, call_remaining_hooks);
        } else {
            call_remaining_hooks();
        }
    }

    unsafe { pg_sys::RegisterSubXactCallback(Some(subxact_callback), ptr::null_mut()) };
    unsafe { pg_sys::RegisterXactCallback(Some(xact_callback), ptr::null_mut()) };

    /// Callback for subtransaction lifecycle events.
    ///
    /// # Safety
    ///
    /// This is called by Postgres via C FFI and must have the `#[pgrx::pg_guard]` attribute.
    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn subxact_callback(
        event: pg_sys::SubXactEvent::Type,
        _my_sub_id: pg_sys::SubTransactionId,
        _parent_sub_id: pg_sys::SubTransactionId,
        _arg: pgrx::void_mut_ptr,
    ) {
        use pg_sys::SubXactEvent::*;

        // NOTE: This does NOT check `export_traces` because these functions clean up anything
        // created by the start hooks.
        if event == SUBXACT_EVENT_ABORT_SUB {
            crate::tracing::subtransaction_abort();
        }
    }

    /// Callback for top-level transaction commit/abort lifecycle events.
    ///
    /// # Safety
    ///
    /// This is called by Postgres via C FFI and must have the `#[pgrx::pg_guard]` attribute.
    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn xact_callback(
        event: pg_sys::XactEvent::Type,
        _arg: pgrx::void_mut_ptr,
    ) {
        use pg_sys::XactEvent::*;

        // NOTE: This does NOT check `export_traces` because these functions clean up anything
        // created by the start hooks.
        match event {
            XACT_EVENT_ABORT | XACT_EVENT_PARALLEL_ABORT => crate::tracing::transaction_abort(),
            XACT_EVENT_COMMIT => crate::tracing::transaction_commit(),
            _ => (),
        }
    }

    static mut HOOK_EXECUTOR_START: pg_sys::ExecutorStart_hook_type = None;
    unsafe {
        HOOK_EXECUTOR_START = pg_sys::ExecutorStart_hook;
        pg_sys::ExecutorStart_hook = Some(executor_start_hook);
    }

    /// Intercepts query executor start in PostgreSQL. This calls our tracing module first.
    ///
    /// # Safety
    ///
    /// This is called by Postgres via C FFI and must have the `#[pgrx::pg_guard]` attribute.
    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn executor_start_hook(qd: *mut pg_sys::QueryDesc, ef: i32) {
        if crate::config::export_traces()
            && let Some(query) = unsafe { qd.as_ref() }
            && let Some(pstmt) = unsafe { query.plannedstmt.as_ref() }
        {
            // This pointer is always populated, but it might not decode as UTF-8.
            let sql = (!query.sourceText.is_null())
                .then(|| unsafe { ffi::CStr::from_ptr(query.sourceText).to_str().ok() })
                .flatten();

            crate::tracing::executor_start(pstmt, sql);
        }

        if let Some(next) = unsafe { HOOK_EXECUTOR_START } {
            unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(qd, ef)) };
        } else {
            unsafe { pg_sys::standard_ExecutorStart(qd, ef) };
        }
    }

    static mut HOOK_EXECUTOR_END: pg_sys::ExecutorEnd_hook_type = None;
    unsafe {
        HOOK_EXECUTOR_END = pg_sys::ExecutorEnd_hook;
        pg_sys::ExecutorEnd_hook = Some(executor_end_hook);
    }

    /// Intercepts query executor end in PostgreSQL. This calls our tracing module last.
    ///
    /// # Safety
    ///
    /// This is called by Postgres via C FFI and must have the `#[pgrx::pg_guard]` attribute.
    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn executor_end_hook(qd: *mut pg_sys::QueryDesc) {
        if let Some(next) = unsafe { HOOK_EXECUTOR_END } {
            unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(qd)) };
        } else {
            unsafe { pg_sys::standard_ExecutorEnd(qd) };
        }

        if crate::config::export_traces()
            && let Some(query) = unsafe { qd.as_ref() }
            && let Some(pstmt) = unsafe { query.plannedstmt.as_ref() }
        {
            // This pointer is always populated, but it might not decode as UTF-8.
            let sql = (!query.sourceText.is_null())
                .then(|| unsafe { ffi::CStr::from_ptr(query.sourceText).to_str().ok() })
                .flatten();

            crate::tracing::executor_end(pstmt, sql);
        }
    }
}
