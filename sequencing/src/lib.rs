// SPDX-License-Identifier: MIT

//! This shared library extension uses numerous Postgres hooks to document when they are called and
//! in what situations.

use pgrx::pg_sys;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{ffi, fs, path, ptr};

#[cfg(any(test, feature = "pg_test"))]
mod tests;

pgrx::pg_module_magic!(name, version);

static SEQUENCE_COUNTER: AtomicU64 = AtomicU64::new(1);
static GUC_TRACE_FILE: pgrx::guc::GucSetting<Option<ffi::CString>> =
    pgrx::guc::GucSetting::<Option<ffi::CString>>::new(None);

#[allow(non_snake_case)]
#[pgrx::pg_guard]
pub extern "C-unwind" fn _PG_init() {
    pgrx::guc::GucRegistry::define_string_guc(
        c"sequencing.trace_file",
        c"",
        c"",
        &GUC_TRACE_FILE,
        pgrx::guc::GucContext::Userset,
        pgrx::guc::GucFlags::empty(),
    );

    static mut HOOK_CLIENT_AUTHENTICATION: pg_sys::libpq::ClientAuthentication_hook_type = None;
    unsafe {
        HOOK_CLIENT_AUTHENTICATION = pg_sys::libpq::ClientAuthentication_hook;
        pg_sys::libpq::ClientAuthentication_hook = Some(client_authentication_hook);

        #[pgrx::pg_guard]
        unsafe extern "C-unwind" fn client_authentication_hook(
            port: *mut pg_sys::libpq::be::Port,
            status: std::os::raw::c_int,
        ) {
            let phase = if status == pg_sys::STATUS_OK as std::os::raw::c_int {
                "STATUS_OK"
            } else {
                "!STATUS_OK"
            };

            log_event("ClientAuthentication_hook", phase, "");

            if let Some(next) = unsafe { HOOK_CLIENT_AUTHENTICATION } {
                unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(port, status)) };
            }
        }
    }

    static mut HOOK_EMIT_LOG: pg_sys::emit_log_hook_type = None;
    unsafe {
        HOOK_EMIT_LOG = pg_sys::emit_log_hook;
        pg_sys::emit_log_hook = Some(emit_log_hook);

        #[pgrx::pg_guard]
        unsafe extern "C-unwind" fn emit_log_hook(edata: *mut pg_sys::ErrorData) {
            if let Some(err) = unsafe { edata.as_mut() } {
                let message = (!err.message.is_null())
                    .then(|| unsafe { ffi::CStr::from_ptr(err.message).to_string_lossy() })
                    .unwrap_or_default();

                log_event(
                    "emit_log_hook",
                    "entry",
                    &format!("elevel={} message={}", err.elevel, message),
                );
            }
            if let Some(next) = unsafe { HOOK_EMIT_LOG } {
                unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(edata)) };
            }
        }
    }

    static mut HOOK_EXECUTOR_CHECK_PERMS: pg_sys::ExecutorCheckPerms_hook_type = None;
    unsafe {
        HOOK_EXECUTOR_CHECK_PERMS = pg_sys::ExecutorCheckPerms_hook;
        pg_sys::ExecutorCheckPerms_hook = Some(executor_check_perms_hook);

        cfg_select! {
            any(feature = "pg13", feature = "pg14", feature = "pg15", feature = "pg16", feature = "pg17") => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn executor_check_perms_hook(
                    rtl: *mut pg_sys::List,
                    eov: bool,
                ) -> bool {
                    log_event("ExecutorCheckPerms_hook", "start", "");
                    let result = if let Some(next) = unsafe { HOOK_EXECUTOR_CHECK_PERMS } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(rtl, eov)) }
                    } else {
                        true
                    };
                    log_event("ExecutorCheckPerms_hook", "end", "");
                    result
                }
            }
            any(feature = "pg18", feature = "pg19") => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn executor_check_perms_hook(
                    _rtl: *mut pg_sys::List,
                    pil: *mut pg_sys::List,
                    eov: bool,
                ) -> bool {
                    log_event("ExecutorCheckPerms_hook", "start", "");
                    let result = if let Some(next) = unsafe { HOOK_EXECUTOR_CHECK_PERMS } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(_rtl, pil, eov)) }
                    } else {
                        true
                    };
                    log_event("ExecutorCheckPerms_hook", "end", "");
                    result
                }
            }
        }
    }

    static mut HOOK_EXECUTOR_END: pg_sys::ExecutorEnd_hook_type = None;
    unsafe {
        HOOK_EXECUTOR_END = pg_sys::ExecutorEnd_hook;
        pg_sys::ExecutorEnd_hook = Some(executor_end_hook);

        #[pgrx::pg_guard]
        unsafe extern "C-unwind" fn executor_end_hook(query: *mut pg_sys::QueryDesc) {
            log_event("ExecutorEnd_hook", "start", "");
            if let Some(next) = unsafe { HOOK_EXECUTOR_END } {
                unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(query)) };
            } else {
                unsafe { pg_sys::standard_ExecutorEnd(query) };
            }
            log_event("ExecutorEnd_hook", "end", "");
        }
    }

    static mut HOOK_EXECUTOR_FINISH: pg_sys::ExecutorFinish_hook_type = None;
    unsafe {
        HOOK_EXECUTOR_FINISH = pg_sys::ExecutorFinish_hook;
        pg_sys::ExecutorFinish_hook = Some(executor_finish_hook);

        #[pgrx::pg_guard]
        unsafe extern "C-unwind" fn executor_finish_hook(query: *mut pg_sys::QueryDesc) {
            log_event("ExecutorFinish_hook", "start", "");
            if let Some(next) = unsafe { HOOK_EXECUTOR_FINISH } {
                unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(query)) };
            } else {
                unsafe { pg_sys::standard_ExecutorFinish(query) };
            }
            log_event("ExecutorFinish_hook", "end", "");
        }
    }

    static mut HOOK_EXECUTOR_RUN: pg_sys::ExecutorRun_hook_type = None;
    unsafe {
        HOOK_EXECUTOR_RUN = pg_sys::ExecutorRun_hook;
        pg_sys::ExecutorRun_hook = Some(executor_run_hook);

        cfg_select! {
            any(feature = "pg13", feature = "pg14", feature = "pg15", feature = "pg16", feature = "pg17") => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn executor_run_hook(
                    query: *mut pg_sys::QueryDesc,
                    dir: pg_sys::ScanDirection::Type,
                    count: u64,
                    once: bool,
                ) {
                    log_event("ExecutorRun_hook", "start", "");
                    if let Some(next) = unsafe { HOOK_EXECUTOR_RUN } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(query, dir, count, once)) };
                    } else {
                        unsafe { pg_sys::standard_ExecutorRun(query, dir, count, once) };
                    }
                    log_event("ExecutorRun_hook", "end", "");
                }
            }
            any(feature = "pg18", feature = "pg19") => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn executor_run_hook(
                    query: *mut pg_sys::QueryDesc,
                    dir: pg_sys::ScanDirection::Type,
                    count: u64,
                ) {
                    log_event("ExecutorRun_hook", "start", "");
                    if let Some(next) = unsafe { HOOK_EXECUTOR_RUN } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(query, dir, count)) };
                    } else {
                        unsafe { pg_sys::standard_ExecutorRun(query, dir, count) };
                    }
                    log_event("ExecutorRun_hook", "end", "");
                }
            }
        }
    }

    static mut HOOK_EXECUTOR_START: pg_sys::ExecutorStart_hook_type = None;
    unsafe {
        HOOK_EXECUTOR_START = pg_sys::ExecutorStart_hook;
        pg_sys::ExecutorStart_hook = Some(executor_start_hook);

        #[pgrx::pg_guard]
        unsafe extern "C-unwind" fn executor_start_hook(
            query: *mut pg_sys::QueryDesc,
            eflags: i32,
        ) {
            log_event("ExecutorStart_hook", "start", "");
            if let Some(next) = unsafe { HOOK_EXECUTOR_START } {
                unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(query, eflags)) };
            } else {
                unsafe { pg_sys::standard_ExecutorStart(query, eflags) };
            }
            log_event("ExecutorStart_hook", "end", "");
        }
    }

    #[allow(dead_code)]
    static mut HOOK_EXPLAIN_ONE_QUERY: pg_sys::ExplainOneQuery_hook_type = None;
    #[cfg(any(feature = "pg17", feature = "pg18", feature = "pg19"))]
    unsafe {
        HOOK_EXPLAIN_ONE_QUERY = pg_sys::ExplainOneQuery_hook;
        pg_sys::ExplainOneQuery_hook = Some(explain_one_query_hook);

        #[pgrx::pg_guard]
        unsafe extern "C-unwind" fn explain_one_query_hook(
            q: *mut pg_sys::Query,
            co: i32,
            ic: *mut pg_sys::IntoClause,
            es: *mut pg_sys::ExplainState,
            qs: *const ffi::c_char,
            pl: pg_sys::ParamListInfo,
            qe: *mut pg_sys::QueryEnvironment,
        ) {
            log_event("ExplainOneQuery_hook", "start", "");
            if let Some(next) = unsafe { HOOK_EXPLAIN_ONE_QUERY } {
                unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(q, co, ic, es, qs, pl, qe)) };
            } else {
                #[cfg(any(feature = "pg17", feature = "pg18", feature = "pg19"))]
                unsafe {
                    pg_sys::standard_ExplainOneQuery(q, co, ic, es, qs, pl, qe);
                }

                #[cfg(not(any(feature = "pg17", feature = "pg18", feature = "pg19")))]
                unreachable!("explain_one_query_hook should only be registered on pg17+");
            }
            log_event("ExplainOneQuery_hook", "end", "");
        }
    }

    static mut HOOK_PLANNER: pg_sys::planner_hook_type = None;
    unsafe {
        HOOK_PLANNER = pg_sys::planner_hook;
        pg_sys::planner_hook = Some(planner_hook);

        cfg_select! {
            any(feature = "pg13", feature = "pg14", feature = "pg15", feature = "pg16", feature = "pg17", feature = "pg18") => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn planner_hook(
                    q: *mut pg_sys::Query,
                    qs: *const ffi::c_char,
                    co: i32,
                    pl: pg_sys::ParamListInfo,
                ) -> *mut pg_sys::PlannedStmt {
                    log_event("planner_hook", "start", "");
                    let result = if let Some(next) = unsafe { HOOK_PLANNER } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(q, qs, co, pl)) }
                    } else {
                        unsafe { pg_sys::standard_planner(q, qs, co, pl) }
                    };
                    log_event("planner_hook", "end", "");
                    result
                }
            }
            any(feature = "pg19") => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn planner_hook(
                    q: *mut pg_sys::Query,
                    qs: *const ffi::c_char,
                    co: i32,
                    pl: pg_sys::ParamListInfo,
                    es: *mut pg_sys::ExplainState,
                ) -> *mut pg_sys::PlannedStmt {
                    log_event("planner_hook", "start", "");
                    let result = if let Some(next) = unsafe { HOOK_PLANNER } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(q, qs, co, pl, es)) }
                    } else {
                        unsafe { pg_sys::standard_planner(q, qs, co, pl, es) }
                    };
                    log_event("planner_hook", "end", "");
                    result
                }
            }
        }
    }

    static mut HOOK_POST_PARSE_ANALYZE: pg_sys::post_parse_analyze_hook_type = None;
    unsafe {
        HOOK_POST_PARSE_ANALYZE = pg_sys::post_parse_analyze_hook;
        pg_sys::post_parse_analyze_hook = Some(post_parse_analyze_hook);

        cfg_select! {
            any(feature = "pg13", feature = "pg14") => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn post_parse_analyze_hook(
                    pstate: *mut pg_sys::ParseState,
                    query: *mut pg_sys::Query,
                ) {
                    log_event("post_parse_analyze_hook", "start", "");
                    if let Some(next) = unsafe { HOOK_POST_PARSE_ANALYZE } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(pstate, query)) };
                    }
                    log_event("post_parse_analyze_hook", "end", "");
                }
            }
            any(feature = "pg15", feature = "pg16", feature = "pg17", feature = "pg18") => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn post_parse_analyze_hook(
                    pstate: *mut pg_sys::ParseState,
                    query: *mut pg_sys::Query,
                    jstate: *mut pg_sys::JumbleState,
                ) {
                    log_event("post_parse_analyze_hook", "start", "");
                    if let Some(next) = unsafe { HOOK_POST_PARSE_ANALYZE } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(pstate, query, jstate)) };
                    }
                    log_event("post_parse_analyze_hook", "end", "");
                }
            }
            any(feature = "pg19") => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn post_parse_analyze_hook(
                    pstate: *mut pg_sys::ParseState,
                    query: *mut pg_sys::Query,
                    jstate: *const pg_sys::JumbleState,
                ) {
                    log_event("post_parse_analyze_hook", "start", "");
                    if let Some(next) = unsafe { HOOK_POST_PARSE_ANALYZE } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(pstate, query, jstate)) };
                    }
                    log_event("post_parse_analyze_hook", "end", "");
                }
            }
        }
    }

    static mut HOOK_PROCESS_UTILITY: pg_sys::ProcessUtility_hook_type = None;
    unsafe {
        HOOK_PROCESS_UTILITY = pg_sys::ProcessUtility_hook;
        pg_sys::ProcessUtility_hook = Some(process_utility_hook);

        cfg_select! {
            feature = "pg13" => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn process_utility_hook(
                    ps: *mut pg_sys::PlannedStmt,
                    qs: *const ffi::c_char,
                    puc: pg_sys::ProcessUtilityContext::Type,
                    pl: pg_sys::ParamListInfo,
                    qe: *mut pg_sys::QueryEnvironment,
                    dr: *mut pg_sys::DestReceiver,
                    qc: *mut pg_sys::QueryCompletion,
                ) {
                    log_event("ProcessUtility_hook", "start", "");
                    if let Some(next) = unsafe { HOOK_PROCESS_UTILITY } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(ps, qs, puc, pl, qe, dr, qc)) };
                    } else {
                        unsafe { pg_sys::standard_ProcessUtility(ps, qs, puc, pl, qe, dr, qc) };
                    }
                    log_event("ProcessUtility_hook", "end", "");
                }
            }
            any(feature = "pg14", feature = "pg15", feature = "pg16", feature = "pg17", feature = "pg18", feature = "pg19") => {
                #[pgrx::pg_guard]
                unsafe extern "C-unwind" fn process_utility_hook(
                    ps: *mut pg_sys::PlannedStmt,
                    qs: *const ffi::c_char,
                    ro: bool,
                    puc: pg_sys::ProcessUtilityContext::Type,
                    pl: pg_sys::ParamListInfo,
                    qe: *mut pg_sys::QueryEnvironment,
                    dr: *mut pg_sys::DestReceiver,
                    qc: *mut pg_sys::QueryCompletion,
                ) {
                    log_event("ProcessUtility_hook", "start", "");
                    if let Some(next) = unsafe { HOOK_PROCESS_UTILITY } {
                        unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(ps, qs, ro, puc, pl, qe, dr, qc)) };
                    } else {
                        unsafe { pg_sys::standard_ProcessUtility(ps, qs, ro, puc, pl, qe, dr, qc) };
                    }
                    log_event("ProcessUtility_hook", "end", "");
                }
            }
        }
    }

    unsafe {
        pg_sys::RegisterSubXactCallback(Some(subxact_callback), ptr::null_mut());

        #[pgrx::pg_guard]
        unsafe extern "C-unwind" fn subxact_callback(
            event: pg_sys::SubXactEvent::Type,
            sub_id: pg_sys::SubTransactionId,
            parent_sub_id: pg_sys::SubTransactionId,
            _arg: pgrx::void_mut_ptr,
        ) {
            use pg_sys::SubXactEvent::*;
            let name = match event {
                SUBXACT_EVENT_START_SUB => "START_SUB",
                SUBXACT_EVENT_COMMIT_SUB => "COMMIT_SUB",
                SUBXACT_EVENT_ABORT_SUB => "ABORT_SUB",
                _ => "OTHER_SUB",
            };
            log_event(
                "subxact_callback",
                name,
                &format!("sub_id={sub_id} parent_sub_id={parent_sub_id}"),
            );
        }
    }

    unsafe {
        pg_sys::RegisterXactCallback(Some(xact_callback), ptr::null_mut());

        #[pgrx::pg_guard]
        unsafe extern "C-unwind" fn xact_callback(
            event: pg_sys::XactEvent::Type,
            _arg: pgrx::void_mut_ptr,
        ) {
            use pg_sys::XactEvent::*;
            let name = match event {
                XACT_EVENT_ABORT => "ABORT",
                XACT_EVENT_COMMIT => "COMMIT",
                XACT_EVENT_PARALLEL_ABORT => "PARALLEL_ABORT",
                XACT_EVENT_PARALLEL_COMMIT => "PARALLEL_COMMIT",
                XACT_EVENT_PARALLEL_PRE_COMMIT => "PARALLEL_PRE_COMMIT",
                XACT_EVENT_PRE_COMMIT => "PRE_COMMIT",
                XACT_EVENT_PRE_PREPARE => "PRE_PREPARE",
                XACT_EVENT_PREPARE => "PREPARE",
                _ => "OTHER",
            };
            log_event("xact_callback", name, "");
        }
    }
}

fn log_event(hook_name: &str, phase: &str, details: &str) {
    let data = unsafe { ffi::CStr::from_ptr(pg_sys::DataDir) };
    let next = SEQUENCE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = unsafe { pg_sys::MyProcPid };
    let filename = GUC_TRACE_FILE
        .get()
        .as_deref()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default.tsv".to_string());
    let filepath = path::Path::new(&data.to_string_lossy().into_owned()).join(filename);

    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&filepath)
    {
        // Format the line first for an atomic write; `writeln!` does not.
        let line = format!("{pid}\t{next}\t{hook_name}\t{phase}\t{details}\n");
        if let Ok(n) = file.write(line.as_bytes()) {
            assert_eq!(n, line.len());
        }
    }
}

#[cfg(test)]
pub mod pg_test {
    pub fn setup(_attributes: Vec<&str>) {}
    pub fn teardown() {}

    #[must_use]
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        let preload = format!("shared_preload_libraries = '{}'", "sequencing");
        vec![
            Box::leak(preload.into_boxed_str()),
            "log_statement = 'all'",
            "max_prepared_transactions = 5",
        ]
    }
}
