// SPDX-License-Identifier: MIT

mod context;

use self::context::{Headers, SpanContext};
use crate::GucString;
use crate::otlp::*;
use pgrx::pg_sys;
use std::cell::{Cell, RefCell};
use std::{ffi, time};

thread_local! {
    static EXECUTION_DEPTH: Cell<usize> = const { Cell::new(0) };
    static GUC_TRACE_CONTEXT: RefCell<Option<SpanContext<'static>>> = const { RefCell::new(None) };
    static IN_GUC_UPDATE: Cell<bool> = const { Cell::new(false) };
    static ACTIVE_STATEMENT_SPAN: RefCell<Option<Span>> = const { RefCell::new(None) };
    static TRANSACTION_SPAN: RefCell<Option<TransactionSpanState>> = const { RefCell::new(None) };
    static LAST_ERROR: RefCell<Option<ErrorRecord>> = const { RefCell::new(None) };
}

struct ErrorRecord {
    sqlstate: String,
    query: Option<String>,
}

struct TransactionSpanState {
    span: Span,
    has_received_context: bool,
    error_code: Option<String>,
}

pub(crate) fn record_error(sqlstate: &str, query: Option<&str>) {
    LAST_ERROR.with(|err| {
        *err.borrow_mut() = Some(ErrorRecord {
            sqlstate: sqlstate.to_string(),
            query: query.map(|s| s.to_string()),
        });
    });
}

pub fn define_guc_variables() {
    use pgrx::guc::{GucContext, GucFlags, GucRegistry};

    static GUC_TRACEPARENT: GucString = GucString::new(None);
    static GUC_TRACESTATE: GucString = GucString::new(None);

    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"pg_otel.traceparent",
            c"W3C traceparent context",
            c"W3C traceparent header for distributed tracing context propagation.",
            &GUC_TRACEPARENT,
            GucContext::Userset,
            GucFlags::empty(),
            Some(check_traceparent),
            Some(assign_traceparent),
            None,
        );

        GucRegistry::define_string_guc_with_hooks(
            c"pg_otel.tracestate",
            c"W3C tracestate context",
            c"W3C tracestate header for distributed tracing context propagation.",
            &GUC_TRACESTATE,
            GucContext::Userset,
            GucFlags::empty(),
            Some(check_tracestate),
            Some(assign_tracestate),
            None,
        );
    }
}

#[pgrx::pg_guc_hook(check)]
fn check_traceparent(value: Option<ffi::CString>) -> Result<(), pgrx::guc::GucCheckError> {
    if let Some(val) = value {
        let s = val
            .to_str()
            .map_err(|e| pgrx::guc::GucCheckError::new(e.to_string()))?;
        if !s.is_empty() {
            let headers = Headers::new(s, None);
            SpanContext::try_from(headers)
                .map_err(|_| pgrx::guc::GucCheckError::new("invalid traceparent format"))?;
        }
    }
    Ok(())
}

#[pgrx::pg_guc_hook(assign)]
fn assign_traceparent(value: Option<ffi::CString>) {
    if IN_GUC_UPDATE.get() {
        return;
    }
    let Some(val) = value else {
        GUC_TRACE_CONTEXT.with(|c| *c.borrow_mut() = None);
        return;
    };
    let s = val.to_string_lossy();
    if s.is_empty() {
        GUC_TRACE_CONTEXT.with(|c| *c.borrow_mut() = None);
        return;
    }
    let headers = Headers::new(&s, None);
    let Ok(ctx) = SpanContext::try_from(headers) else {
        return;
    };
    let ctx_owned = ctx.into_owned();

    let mut applied_to_xact = false;
    TRANSACTION_SPAN.with(|ts| {
        if let Some(xact) = ts.borrow_mut().as_mut() {
            if !xact.has_received_context {
                xact.span.trace_id = ctx_owned.trace.id.to_bytes().to_vec();
                xact.span.parent_span_id = ctx_owned.id.to_bytes().to_vec();
                xact.has_received_context = true;
                applied_to_xact = true;
            }
        }
    });

    if applied_to_xact {
        GUC_TRACE_CONTEXT.with(|c| *c.borrow_mut() = None);
        clear_guc_traceparent();
    } else {
        GUC_TRACE_CONTEXT.with(|c| *c.borrow_mut() = Some(ctx_owned));
    }
}

#[pgrx::pg_guc_hook(check)]
fn check_tracestate(_value: Option<ffi::CString>) -> Result<(), pgrx::guc::GucCheckError> {
    Ok(())
}

#[pgrx::pg_guc_hook(assign)]
fn assign_tracestate(_value: Option<ffi::CString>) {}

fn clear_guc_traceparent() {
    IN_GUC_UPDATE.set(true);
    unsafe {
        pg_sys::SetConfigOption(
            c"pg_otel.traceparent".as_ptr(),
            c"".as_ptr(),
            pg_sys::GucContext::PGC_USERSET,
            pg_sys::GucSource::PGC_S_SESSION,
        );
    }
    IN_GUC_UPDATE.set(false);
}

fn to_unix_nano(time: time::SystemTime) -> u64 {
    time.duration_since(time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    unsafe {
        pg_sys::pg_strong_random(buf.as_mut_ptr() as *mut ffi::c_void, N);
    }
    buf
}

fn export_span(span: &Span) {
    let mut buffer = crate::BytesMut::new();
    if let Err(e) = prost::Message::encode(span, &mut buffer) {
        pgrx::warning!("pg_otel: failed to encode span: {e}");
        return;
    }
    crate::exporter::send_one(&buffer);
}

fn format_traceparent(trace_id: &[u8], span_id: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(55);
    s.push_str("00-");
    for b in trace_id {
        write!(s, "{:02x}", b).unwrap();
    }
    s.push('-');
    for b in span_id {
        write!(s, "{:02x}", b).unwrap();
    }
    s.push_str("-01");
    s
}

fn get_operation_name(pstmt: Option<&pg_sys::PlannedStmt>, sql: Option<&str>) -> String {
    if let Some(ps) = pstmt {
        match ps.commandType {
            pg_sys::CmdType::CMD_SELECT => return "SELECT".to_string(),
            pg_sys::CmdType::CMD_UPDATE => return "UPDATE".to_string(),
            pg_sys::CmdType::CMD_INSERT => return "INSERT".to_string(),
            pg_sys::CmdType::CMD_DELETE => return "DELETE".to_string(),
            pg_sys::CmdType::CMD_MERGE => return "MERGE".to_string(),
            pg_sys::CmdType::CMD_UTILITY if !ps.utilityStmt.is_null() => {
                let tag = unsafe { pg_sys::CreateCommandTag(ps.utilityStmt) };
                let tag_name = unsafe { ffi::CStr::from_ptr(pg_sys::GetCommandTagName(tag)) }
                    .to_str()
                    .unwrap_or_default();
                let op = tag_name.split_whitespace().next().unwrap_or(tag_name);
                if !op.is_empty() {
                    return op.to_ascii_uppercase();
                }
            }
            _ => (),
        }
    }

    if let Some(s) = sql {
        let mut text = s.trim();
        while !text.is_empty() {
            if text.starts_with("/*") {
                if let Some((_, rest)) = text.split_once("*/") {
                    text = rest.trim();
                    continue;
                }
            }
            if text.starts_with("--") {
                if let Some((_, rest)) = text.split_once('\n') {
                    text = rest.trim();
                    continue;
                }
            }
            break;
        }

        if let Some(word) = text.split_whitespace().next() {
            let clean_word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            let upper = clean_word.to_ascii_uppercase();
            if upper == "WITH" {
                for tok in text.split_whitespace().skip(1) {
                    let clean = tok
                        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
                        .to_ascii_uppercase();
                    if matches!(
                        clean.as_str(),
                        "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "MERGE"
                    ) {
                        return clean;
                    }
                }
                return "SELECT".to_string();
            }
            if !upper.is_empty() {
                return upper;
            }
        }
    }

    "UNKNOWN".to_string()
}

fn is_variable_set(pstmt: Option<&pg_sys::PlannedStmt>, sql: Option<&str>) -> bool {
    if let Some(ps) = pstmt
        && !ps.utilityStmt.is_null()
        && unsafe { (ps.utilityStmt as *const pg_sys::Node).as_ref() }
            .map(|n| n.type_ == pg_sys::NodeTag::T_VariableSetStmt)
            .unwrap_or(false)
    {
        return true;
    }
    if let Some(s) = sql {
        let mut text = s.trim();
        while text.starts_with("/*") {
            if let Some((_, rest)) = text.split_once("*/") {
                text = rest.trim();
            } else {
                break;
            }
        }
        let upper = text.to_ascii_uppercase();
        if upper.starts_with("SET ") || upper.starts_with("RESET ") {
            return true;
        }
    }
    false
}

fn get_transaction_stmt_kind(
    pstmt: Option<&pg_sys::PlannedStmt>,
    sql: Option<&str>,
) -> Option<pg_sys::TransactionStmtKind::Type> {
    if let Some(ps) = pstmt
        && !ps.utilityStmt.is_null()
        && unsafe { (ps.utilityStmt as *const pg_sys::Node).as_ref() }
            .map(|n| n.type_ == pg_sys::NodeTag::T_TransactionStmt)
            .unwrap_or(false)
    {
        let xact = unsafe { &*(ps.utilityStmt as *const pg_sys::TransactionStmt) };
        return Some(xact.kind);
    }
    if let Some(s) = sql {
        let mut text = s.trim();
        while text.starts_with("/*") {
            if let Some((_, rest)) = text.split_once("*/") {
                text = rest.trim();
            } else {
                break;
            }
        }
        let upper = text.to_ascii_uppercase();
        if upper.starts_with("BEGIN") || upper.starts_with("START TRANSACTION") {
            return Some(pg_sys::TransactionStmtKind::TRANS_STMT_BEGIN);
        }
        if upper.starts_with("COMMIT") || upper.starts_with("END") {
            return Some(pg_sys::TransactionStmtKind::TRANS_STMT_COMMIT);
        }
        if upper.starts_with("ROLLBACK") || upper.starts_with("ABORT") {
            return Some(pg_sys::TransactionStmtKind::TRANS_STMT_ROLLBACK);
        }
    }
    None
}

pub fn install_hooks() {
    debug_assert!(crate::assert_postmaster_startup());

    static mut HOOK_PROCESS_UTILITY: pg_sys::ProcessUtility_hook_type = None;
    unsafe {
        HOOK_PROCESS_UTILITY = pg_sys::ProcessUtility_hook;
        pg_sys::ProcessUtility_hook = Some(process_utility_hook);
    }

    #[pgrx::pg_guard]
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
        // SAFETY: HOOK_PROCESS_UTILITY is assigned above, during startup; it is safe to read here.
        // SAFETY: pg_guard_ffi_boundary handles any Postgres error that occurs inside the next hook.
        let call_remaining_hooks = || unsafe {
            if let Some(next) = HOOK_PROCESS_UTILITY {
                pg_sys::ffi::pg_guard_ffi_boundary(|| next(ps, qs, ro, pc, pl, qe, dr, qc));
            } else {
                pg_sys::standard_ProcessUtility(ps, qs, ro, pc, pl, qe, dr, qc);
            }
        };

        if crate::ENABLED.get().spans() {
            process_utility(
                (!ps.is_null() && ps.is_aligned()).then(|| unsafe { ps.as_ref_unchecked() }),
                (!qs.is_null()).then(|| unsafe { ffi::CStr::from_ptr(qs) }),
                call_remaining_hooks,
            );
        } else {
            call_remaining_hooks();
        }
    }

    unsafe { pg_sys::RegisterXactCallback(Some(xact_callback), std::ptr::null_mut()) };
    unsafe { pg_sys::RegisterSubXactCallback(Some(subxact_callback), std::ptr::null_mut()) };

    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn xact_callback(
        event: pg_sys::XactEvent::Type,
        _arg: pgrx::void_mut_ptr,
    ) {
        use pg_sys::XactEvent::*;
        match event {
            XACT_EVENT_ABORT | XACT_EVENT_PARALLEL_ABORT => transaction_abort(),
            XACT_EVENT_COMMIT => transaction_commit(),
            _ => (),
        }
    }

    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn subxact_callback(
        event: pg_sys::SubXactEvent::Type,
        _my_sub_id: pg_sys::SubTransactionId,
        _parent_sub_id: pg_sys::SubTransactionId,
        _arg: pgrx::void_mut_ptr,
    ) {
        use pg_sys::SubXactEvent::*;
        if event == SUBXACT_EVENT_ABORT_SUB {
            subxact_abort();
        }
    }

    static mut HOOK_EXECUTOR_START: pg_sys::ExecutorStart_hook_type = None;
    unsafe {
        HOOK_EXECUTOR_START = pg_sys::ExecutorStart_hook;
        pg_sys::ExecutorStart_hook = Some(executor_start_hook);
    }

    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn executor_start_hook(qd: *mut pg_sys::QueryDesc, ef: i32) {
        if crate::ENABLED.get().spans() {
            executor_start(
                (!qd.is_null() && qd.is_aligned()).then(|| unsafe { qd.as_ref_unchecked() }),
            );
        }

        // SAFETY: HOOK_EXECUTOR_START is assigned above, during startup; it is safe to read here.
        // SAFETY: pg_guard_ffi_boundary handles any Postgres error that occurs inside the next hook.
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

    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn executor_end_hook(qd: *mut pg_sys::QueryDesc) {
        if let Some(next) = unsafe { HOOK_EXECUTOR_END } {
            unsafe { pg_sys::ffi::pg_guard_ffi_boundary(|| next(qd)) };
        } else {
            unsafe { pg_sys::standard_ExecutorEnd(qd) };
        }

        if crate::ENABLED.get().spans() {
            executor_end(
                (!qd.is_null() && qd.is_aligned()).then(|| unsafe { qd.as_ref_unchecked() }),
            );
        }
    }
}

fn executor_start(query: Option<&pg_sys::QueryDesc>) {
    let now = time::SystemTime::now();
    let Some(qd) = query else { return };

    let is_parallel_worker = unsafe { pg_sys::ParallelWorkerNumber >= 0 };
    if is_parallel_worker {
        if let Some(ctx) = GUC_TRACE_CONTEXT.with(|c| c.borrow_mut().take()) {
            let sql = if !qd.sourceText.is_null() {
                unsafe { ffi::CStr::from_ptr(qd.sourceText).to_str().ok() }
            } else {
                None
            };
            let pstmt = if !qd.plannedstmt.is_null() {
                unsafe { qd.plannedstmt.as_ref() }
            } else {
                None
            };
            let op_name = get_operation_name(pstmt, sql);
            let span = Span::build()
                .name(op_name.clone())
                .kind(SpanKind::Server as i32)
                .trace_id(ctx.trace.id.to_bytes().to_vec())
                .span_id(random_bytes::<8>().to_vec())
                .parent_span_id(ctx.id.to_bytes().to_vec())
                .start_time_unix_nano(to_unix_nano(now))
                .attributes(vec![
                    KeyValue::new_string("db.operation.name", op_name),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ])
                .finish();
            ACTIVE_STATEMENT_SPAN.with(|s| *s.borrow_mut() = Some(span));
            EXECUTION_DEPTH.set(1);
        }
        return;
    }

    let is_commit_or_rollback = ACTIVE_STATEMENT_SPAN.with(|s| {
        s.borrow().as_ref().map_or(false, |span| {
            span.name == "COMMIT" || span.name == "ROLLBACK"
        })
    });
    let depth = EXECUTION_DEPTH.get();
    if depth > 0 || is_commit_or_rollback {
        EXECUTION_DEPTH.set(depth + 1);
        return;
    }
    EXECUTION_DEPTH.set(1);

    let sql = if !qd.sourceText.is_null() {
        unsafe { ffi::CStr::from_ptr(qd.sourceText).to_str().ok() }
    } else {
        None
    };
    let pstmt = if !qd.plannedstmt.is_null() {
        unsafe { qd.plannedstmt.as_ref() }
    } else {
        None
    };

    let (stmt_begin, stmt_len) = pstmt.map_or((-1, 0), |p| (p.stmt_location, p.stmt_len));
    let comment_ctx = sql
        .and_then(|s| Headers::from_statement(s, stmt_begin, stmt_len))
        .and_then(|h| SpanContext::try_from(h).ok());
    let guc_ctx = GUC_TRACE_CONTEXT.with(|c| c.borrow_mut().take());

    let own_ctx = comment_ctx.or(guc_ctx);
    clear_guc_traceparent();

    let mut links = Vec::new();
    let (trace_id, parent_span_id) = TRANSACTION_SPAN.with(|ts| {
        if let Some(xact) = ts.borrow().as_ref() {
            if let Some(ctx) = own_ctx {
                links.push(
                    Link::build()
                        .trace_id(xact.span.trace_id.clone())
                        .span_id(xact.span.span_id.clone())
                        .finish(),
                );
                (ctx.trace.id.to_bytes().to_vec(), ctx.id.to_bytes().to_vec())
            } else {
                (xact.span.trace_id.clone(), xact.span.span_id.clone())
            }
        } else if let Some(ctx) = own_ctx {
            (ctx.trace.id.to_bytes().to_vec(), ctx.id.to_bytes().to_vec())
        } else {
            (random_bytes::<16>().to_vec(), vec![])
        }
    });

    let op_name = get_operation_name(pstmt, sql);
    let span_id = random_bytes::<8>().to_vec();

    if pstmt.map_or(false, |p| p.parallelModeNeeded) {
        let tp = format_traceparent(&trace_id, &span_id);
        IN_GUC_UPDATE.set(true);
        unsafe {
            let val = ffi::CString::new(tp).unwrap();
            pg_sys::SetConfigOption(
                c"pg_otel.traceparent".as_ptr(),
                val.as_ptr(),
                pg_sys::GucContext::PGC_USERSET,
                pg_sys::GucSource::PGC_S_SESSION,
            );
        }
        IN_GUC_UPDATE.set(false);
    }

    let span = Span::build()
        .name(op_name.clone())
        .kind(SpanKind::Server as i32)
        .trace_id(trace_id)
        .span_id(span_id)
        .parent_span_id(parent_span_id)
        .start_time_unix_nano(to_unix_nano(now))
        .links(links)
        .attributes(vec![
            KeyValue::new_string("db.operation.name", op_name),
            KeyValue::new_string("db.system.name", "postgresql"),
        ])
        .finish();

    ACTIVE_STATEMENT_SPAN.with(|s| *s.borrow_mut() = Some(span));
}

fn executor_end(query: Option<&pg_sys::QueryDesc>) {
    let is_parallel_worker = unsafe { pg_sys::ParallelWorkerNumber >= 0 };
    if is_parallel_worker {
        EXECUTION_DEPTH.set(0);
        if let Some(mut span) = ACTIVE_STATEMENT_SPAN.with(|s| s.borrow_mut().take()) {
            span.end_time_unix_nano = to_unix_nano(time::SystemTime::now());
            export_span(&span);
        }
        return;
    }

    let depth = EXECUTION_DEPTH.get();
    if depth > 0 {
        EXECUTION_DEPTH.set(depth - 1);
        let is_commit_or_rollback = ACTIVE_STATEMENT_SPAN.with(|s| {
            s.borrow().as_ref().map_or(false, |span| {
                span.name == "COMMIT" || span.name == "ROLLBACK"
            })
        });
        if depth - 1 == 0 && !is_commit_or_rollback {
            if let Some(qd) = query
                && let Some(ps) = unsafe { qd.plannedstmt.as_ref() }
                && ps.parallelModeNeeded
            {
                clear_guc_traceparent();
            }

            if let Some(mut span) = ACTIVE_STATEMENT_SPAN.with(|s| s.borrow_mut().take()) {
                span.end_time_unix_nano = to_unix_nano(time::SystemTime::now());
                export_span(&span);
            }
        }
    }
}

fn process_utility<F>(pstmt: Option<&pg_sys::PlannedStmt>, sql: Option<&ffi::CStr>, hooks: F)
where
    F: FnOnce(),
{
    let now = time::SystemTime::now();
    let sql_str = sql.and_then(|s| s.to_str().ok());

    if is_variable_set(pstmt, sql_str) {
        hooks();
        return;
    }

    let xact_kind = get_transaction_stmt_kind(pstmt, sql_str);
    match xact_kind {
        Some(pg_sys::TransactionStmtKind::TRANS_STMT_BEGIN)
        | Some(pg_sys::TransactionStmtKind::TRANS_STMT_START) => {
            let (stmt_begin, stmt_len) = pstmt.map_or((-1, 0), |p| (p.stmt_location, p.stmt_len));
            let comment_ctx = sql_str
                .and_then(|s| Headers::from_statement(s, stmt_begin, stmt_len))
                .and_then(|h| SpanContext::try_from(h).ok())
                .map(|c| c.into_owned());
            let guc_ctx = GUC_TRACE_CONTEXT.with(|c| c.borrow_mut().take());

            let (trace_id, parent_span_id, has_ctx) = if let Some(ctx) = comment_ctx {
                (
                    ctx.trace.id.to_bytes().to_vec(),
                    ctx.id.to_bytes().to_vec(),
                    true,
                )
            } else if let Some(ctx) = guc_ctx {
                (
                    ctx.trace.id.to_bytes().to_vec(),
                    ctx.id.to_bytes().to_vec(),
                    true,
                )
            } else {
                (random_bytes::<16>().to_vec(), vec![], false)
            };

            clear_guc_traceparent();

            let span_id = random_bytes::<8>().to_vec();
            let span = Span::build()
                .name("TRANSACTION")
                .kind(SpanKind::Server as i32)
                .trace_id(trace_id)
                .span_id(span_id)
                .parent_span_id(parent_span_id)
                .start_time_unix_nano(to_unix_nano(now))
                .attributes(vec![
                    KeyValue::new_string("db.operation.name", "TRANSACTION"),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ])
                .finish();

            TRANSACTION_SPAN.with(|ts| {
                *ts.borrow_mut() = Some(TransactionSpanState {
                    span,
                    has_received_context: has_ctx,
                    error_code: None,
                });
            });

            hooks();
            return;
        }
        Some(pg_sys::TransactionStmtKind::TRANS_STMT_COMMIT) => {
            let (trace_id, parent_span_id) = TRANSACTION_SPAN.with(|ts| {
                if let Some(xact) = ts.borrow().as_ref() {
                    (xact.span.trace_id.clone(), xact.span.span_id.clone())
                } else {
                    (random_bytes::<16>().to_vec(), vec![])
                }
            });
            let span_id = random_bytes::<8>().to_vec();
            let span = Span::build()
                .name("COMMIT")
                .kind(SpanKind::Server as i32)
                .trace_id(trace_id)
                .span_id(span_id)
                .parent_span_id(parent_span_id)
                .start_time_unix_nano(to_unix_nano(now))
                .attributes(vec![
                    KeyValue::new_string("db.operation.name", "COMMIT"),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ])
                .finish();
            ACTIVE_STATEMENT_SPAN.with(|s| *s.borrow_mut() = Some(span));
            hooks();
            return;
        }
        Some(pg_sys::TransactionStmtKind::TRANS_STMT_ROLLBACK) => {
            let (trace_id, parent_span_id) = TRANSACTION_SPAN.with(|ts| {
                if let Some(xact) = ts.borrow().as_ref() {
                    (xact.span.trace_id.clone(), xact.span.span_id.clone())
                } else {
                    (random_bytes::<16>().to_vec(), vec![])
                }
            });
            let span_id = random_bytes::<8>().to_vec();
            let span = Span::build()
                .name("ROLLBACK")
                .kind(SpanKind::Server as i32)
                .trace_id(trace_id)
                .span_id(span_id)
                .parent_span_id(parent_span_id)
                .start_time_unix_nano(to_unix_nano(now))
                .attributes(vec![
                    KeyValue::new_string("db.operation.name", "ROLLBACK"),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ])
                .finish();
            ACTIVE_STATEMENT_SPAN.with(|s| *s.borrow_mut() = Some(span));
            hooks();

            if let Some(mut span) = ACTIVE_STATEMENT_SPAN.with(|s| s.borrow_mut().take()) {
                span.end_time_unix_nano = to_unix_nano(time::SystemTime::now());
                export_span(&span);
            }
            if let Some(mut xact) = TRANSACTION_SPAN.with(|ts| ts.borrow_mut().take()) {
                if let Some(code) = &xact.error_code {
                    xact.span.status = Some(SpanStatus::error(""));
                    xact.span
                        .attributes
                        .push(KeyValue::new_string("db.response.status_code", code));
                }
                xact.span.end_time_unix_nano = to_unix_nano(time::SystemTime::now());
                export_span(&xact.span);
            }
            return;
        }
        _ => (),
    }

    let depth = EXECUTION_DEPTH.get();
    if depth > 0 {
        EXECUTION_DEPTH.set(depth + 1);
        hooks();
        EXECUTION_DEPTH.set(depth);
        return;
    }
    EXECUTION_DEPTH.set(1);

    let (stmt_begin, stmt_len) = pstmt.map_or((-1, 0), |p| (p.stmt_location, p.stmt_len));
    let comment_ctx = sql_str
        .and_then(|s| Headers::from_statement(s, stmt_begin, stmt_len))
        .and_then(|h| SpanContext::try_from(h).ok())
        .map(|c| c.into_owned());
    let guc_ctx = GUC_TRACE_CONTEXT.with(|c| c.borrow_mut().take());

    let own_ctx = comment_ctx.or(guc_ctx);
    clear_guc_traceparent();

    let mut links = Vec::new();
    let (trace_id, parent_span_id) = TRANSACTION_SPAN.with(|ts| {
        if let Some(xact) = ts.borrow().as_ref() {
            if let Some(ctx) = own_ctx {
                links.push(
                    Link::build()
                        .trace_id(xact.span.trace_id.clone())
                        .span_id(xact.span.span_id.clone())
                        .finish(),
                );
                (ctx.trace.id.to_bytes().to_vec(), ctx.id.to_bytes().to_vec())
            } else {
                (xact.span.trace_id.clone(), xact.span.span_id.clone())
            }
        } else if let Some(ctx) = own_ctx {
            (ctx.trace.id.to_bytes().to_vec(), ctx.id.to_bytes().to_vec())
        } else {
            (random_bytes::<16>().to_vec(), vec![])
        }
    });

    let op_name = get_operation_name(pstmt, sql_str);
    let span_id = random_bytes::<8>().to_vec();
    let span = Span::build()
        .name(op_name.clone())
        .kind(SpanKind::Server as i32)
        .trace_id(trace_id)
        .span_id(span_id)
        .parent_span_id(parent_span_id)
        .start_time_unix_nano(to_unix_nano(now))
        .links(links)
        .attributes(vec![
            KeyValue::new_string("db.operation.name", op_name),
            KeyValue::new_string("db.system.name", "postgresql"),
        ])
        .finish();

    ACTIVE_STATEMENT_SPAN.with(|s| *s.borrow_mut() = Some(span));

    hooks();

    EXECUTION_DEPTH.set(0);
    if let Some(mut span) = ACTIVE_STATEMENT_SPAN.with(|s| s.borrow_mut().take()) {
        span.end_time_unix_nano = to_unix_nano(time::SystemTime::now());
        export_span(&span);
    }
}

fn transaction_abort() {
    EXECUTION_DEPTH.set(0);
    let now = time::SystemTime::now();
    let last_error = LAST_ERROR.with(|err| err.borrow_mut().take());
    let in_xact_block = unsafe { pg_sys::IsTransactionBlock() };

    let is_commit_or_rollback = ACTIVE_STATEMENT_SPAN.with(|s| {
        s.borrow().as_ref().map_or(false, |span| {
            span.name == "COMMIT" || span.name == "ROLLBACK"
        })
    });
    let is_ending_xact = is_commit_or_rollback || !in_xact_block;

    if let Some(err_record) = last_error {
        let code = err_record.sqlstate;
        let active_span = ACTIVE_STATEMENT_SPAN.with(|s| s.borrow_mut().take());
        if let Some(mut span) = active_span {
            span.status = Some(SpanStatus::error(""));
            span.attributes
                .push(KeyValue::new_string("db.response.status_code", &code));
            span.end_time_unix_nano = to_unix_nano(now);
            export_span(&span);
        } else {
            let query_str = err_record.query.as_deref().or_else(|| unsafe {
                if !pg_sys::debug_query_string.is_null() {
                    ffi::CStr::from_ptr(pg_sys::debug_query_string)
                        .to_str()
                        .ok()
                } else {
                    None
                }
            });
            if let Some(qs) = query_str {
                let op_name = get_operation_name(None, Some(qs));
                if !op_name.is_empty() && op_name != "UNKNOWN" {
                    let mut trace_id = random_bytes::<16>().to_vec();
                    let mut parent_span_id = vec![];
                    let span_id = random_bytes::<8>().to_vec();

                    TRANSACTION_SPAN.with(|ts| {
                        if let Some(xact) = ts.borrow().as_ref() {
                            trace_id = xact.span.trace_id.clone();
                            parent_span_id = xact.span.span_id.clone();
                        }
                    });

                    let span = Span::build()
                        .name(op_name.clone())
                        .kind(SpanKind::Server as i32)
                        .trace_id(trace_id)
                        .span_id(span_id)
                        .parent_span_id(parent_span_id)
                        .status(SpanStatus::error(""))
                        .start_time_unix_nano(to_unix_nano(now))
                        .end_time_unix_nano(to_unix_nano(now))
                        .attributes(vec![
                            KeyValue::new_string("db.operation.name", op_name),
                            KeyValue::new_string("db.system.name", "postgresql"),
                            KeyValue::new_string("db.response.status_code", &code),
                        ])
                        .finish();
                    export_span(&span);
                }
            }
        }

        if in_xact_block || is_commit_or_rollback {
            TRANSACTION_SPAN.with(|ts| {
                if let Some(xact) = ts.borrow_mut().as_mut() {
                    xact.span.status = Some(SpanStatus::error(""));
                    xact.error_code = Some(code);
                }
            });
        }
    } else {
        if let Some(mut span) = ACTIVE_STATEMENT_SPAN.with(|s| s.borrow_mut().take()) {
            span.end_time_unix_nano = to_unix_nano(now);
            export_span(&span);
        }
    }

    if is_ending_xact {
        if let Some(mut xact) = TRANSACTION_SPAN.with(|ts| ts.borrow_mut().take()) {
            if let Some(code) = &xact.error_code {
                xact.span.status = Some(SpanStatus::error(""));
                xact.span
                    .attributes
                    .push(KeyValue::new_string("db.response.status_code", code));
            }
            xact.span.end_time_unix_nano = to_unix_nano(now);
            export_span(&xact.span);
        }
    }
}

fn subxact_abort() {}

fn transaction_commit() {
    EXECUTION_DEPTH.set(0);
    let now = time::SystemTime::now();
    if let Some(mut span) = ACTIVE_STATEMENT_SPAN.with(|s| s.borrow_mut().take()) {
        span.end_time_unix_nano = to_unix_nano(now);
        export_span(&span);
    }
    if let Some(mut xact) = TRANSACTION_SPAN.with(|ts| ts.borrow_mut().take()) {
        xact.span.end_time_unix_nano = to_unix_nano(now);
        export_span(&xact.span);
    }
}

#[cfg(any(test, feature = "pg_test"))]
mod tests;
