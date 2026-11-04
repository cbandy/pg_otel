// SPDX-License-Identifier: MIT

use crate::context::{Headers, SpanContext, SpanId, TraceFlags, TraceId};
use crate::otlp::*;
use crate::{GucInt32, GucString};
use core::cell::{Cell, RefCell};
use core::ptr;
use pgrx::pg_sys::{self, PgNode};
use std::{ffi, rc::Rc, time};

static GUC_MAX_NESTED_STATEMENTS: GucInt32 = GucInt32::new(50);
static GUC_TRACE_NESTED_STATEMENTS: GucInt32 = GucInt32::new(0);
static GUC_TRACEPARENT: GucString = GucString::new(None);
static GUC_TRACESTATE: GucString = GucString::new(None);

thread_local! {
    /// Tracks the starting byte offset of the unparsed remainder in a multi-statement simple query batch.
    ///
    /// Since PostgreSQL 18, `PlannedStmt::stmt_location` advances past leading comments to the first SQL keyword.
    /// The value here marks where the previous statement's terminating delimiter ended so leading comments can be recovered without backward scanning.
    /// It also anchors synthetic error spans when a batch aborts.
    static BATCH_CURSOR: Cell<usize> = const { Cell::new(0) };
}

/// Advances the batch SQL cursor to the position immediately after the completed statement's terminating semicolon.
///
/// PostgreSQL documents on `PlannedStmt` in `plannodes.h` that:
/// - `stmt_location` is the start byte offset, or -1 if unknown.
/// - `stmt_len` is the length in bytes; `0` means "rest of string".
///
/// The definitions in `gram.y` ensure that when `stmt_len > 0`,
/// `stmt_location + stmt_len` points directly at the terminating `;` token.
///
/// When the statement extends to EOF or reaches the end of the query string,
/// the cursor resets to `0` ahead of the next query.
fn batch_advance_cursor(sql: &str, stmt_location: i32, stmt_len: i32) {
    if stmt_location < 0 || stmt_len <= 0 {
        BATCH_CURSOR.set(0);
        return;
    }
    let stmt_end = (stmt_location + stmt_len) as usize;
    let next_pos = if sql.as_bytes().get(stmt_end) == Some(&b';') {
        stmt_end + 1
    } else {
        stmt_end
    };

    BATCH_CURSOR.set(if next_pos >= sql.len() { 0 } else { next_pos });
}

/// Slices a statement from the batch SQL text using engine byte offsets.
///
/// When `stmt_location` is negative (e.g. parse failures), slicing begins at `BATCH_CURSOR`.
/// When `stmt_len` is non-positive, slicing extends to the end of `sql`.
fn batch_statement_slice(sql: &str, stmt_location: i32, stmt_len: i32) -> Option<&str> {
    let start = if stmt_location >= 0 {
        stmt_location as usize
    } else {
        BATCH_CURSOR.get()
    };
    let end = if stmt_location >= 0 && stmt_len > 0 {
        (stmt_location + stmt_len) as usize
    } else {
        sql.len()
    };

    sql.get(start..end)
}

#[derive(Debug)]
struct ActiveStatement {
    span: Span,
    context: SpanContext,
    depth: usize,
    scope_budget: Cell<i32>,
}

#[derive(Debug)]
struct TransactionSpanState {
    span: Span,
    context: SpanContext,
    has_received_context: bool,
}

#[derive(Debug)]
enum StackSpan {
    Transaction(TransactionSpanState),
    Statement(ActiveStatement),
}

impl StackSpan {
    fn span(&self) -> &Span {
        match self {
            StackSpan::Transaction(t) => &t.span,
            StackSpan::Statement(s) => &s.span,
        }
    }

    fn span_mut(&mut self) -> &mut Span {
        match self {
            StackSpan::Transaction(t) => &mut t.span,
            StackSpan::Statement(s) => &mut s.span,
        }
    }

    fn context(&self) -> SpanContext {
        match self {
            StackSpan::Transaction(t) => t.context.clone(),
            StackSpan::Statement(s) => s.context.clone(),
        }
    }

    fn depth(&self) -> usize {
        match self {
            StackSpan::Transaction(_) => 0,
            StackSpan::Statement(s) => s.depth,
        }
    }

    fn is_transaction(&self) -> bool {
        matches!(self, StackSpan::Transaction(_))
    }

    fn is_statement(&self) -> bool {
        matches!(self, StackSpan::Statement(_))
    }

    fn is_commit(&self) -> bool {
        self.span().name == "COMMIT"
    }

    fn is_rollback(&self) -> bool {
        self.span().name == "ROLLBACK"
    }

    fn attribute(&mut self, kv: KeyValue) {
        self.span_mut().attribute(kv);
    }

    fn finish_and_export(self, end_time: time::SystemTime) {
        let (mut span, context) = match self {
            StackSpan::Transaction(t) => (t.span, t.context),
            StackSpan::Statement(s) => (s.span, s.context),
        };
        span.span_id = context.span_id.into();
        span.trace_id = context.trace_id.into();
        span.flags = context.trace_flags.into();
        span.trace_state = context.trace_state.to_string();
        span.end(end_time);
        export_span(&span);
    }
}

fn propagate_context(
    incoming: Option<SpanContext>,
    existing: Option<SpanContext>,
) -> (SpanContext, SpanBuilder) {
    let builder = Span::build().kind(SpanKind::Server as i32);
    let span_id = SpanId::random();

    match (incoming, existing) {
        (None, None) => (
            SpanContext {
                span_id,
                trace_id: TraceId::random(),
                trace_flags: TraceFlags::W3C_RANDOM | TraceFlags::W3C_SAMPLED,
                trace_state: Rc::default(),
            },
            builder,
        ),
        (Some(i), Some(e)) => (
            SpanContext {
                span_id,
                trace_id: i.trace_id,
                trace_flags: i.trace_flags,
                trace_state: i.trace_state,
            },
            builder.parent_span_id(Some(i.span_id)).links([Link::build()
                .span_id(e.span_id)
                .trace_id(e.trace_id)
                .flags(e.trace_flags)
                .trace_state(&*e.trace_state)
                .finish()]),
        ),
        (Some(i), None) => (
            SpanContext {
                span_id,
                trace_id: i.trace_id,
                trace_flags: i.trace_flags,
                trace_state: i.trace_state,
            },
            builder.parent_span_id(Some(i.span_id)),
        ),
        (None, Some(e)) => (
            SpanContext {
                span_id,
                trace_id: e.trace_id,
                trace_flags: e.trace_flags,
                trace_state: e.trace_state,
            },
            builder.parent_span_id(Some(e.span_id)),
        ),
    }
}

fn start_nested_span(parent_ctx: &SpanContext) -> (SpanContext, SpanBuilder) {
    let child_ctx = SpanContext {
        span_id: SpanId::random(),
        trace_id: parent_ctx.trace_id,
        trace_flags: parent_ctx.trace_flags,
        trace_state: parent_ctx.trace_state.clone(),
    };
    let builder = Span::build()
        .kind(SpanKind::Internal as i32)
        .parent_span_id(Some(parent_ctx.span_id));

    (child_ctx, builder)
}

struct ErrorRecord {
    sqlstate: String,
    query: Option<String>,
}

thread_local! {
    static EXECUTION_DEPTH: Cell<usize> = const { Cell::new(0) };
    static IN_GUC_UPDATE: Cell<bool> = const { Cell::new(false) };
    static SPAN_STACK: RefCell<Vec<StackSpan>> = const { RefCell::new(Vec::new()) };
    static REMAINING_BUDGET: Cell<i32> = const { Cell::new(0) };
    static DROPPED_NESTED_STATEMENTS: Cell<usize> = const { Cell::new(0) };
    static LAST_ERROR: RefCell<Option<ErrorRecord>> = const { RefCell::new(None) };
}

pub(crate) fn current_span_context() -> Option<SpanContext> {
    SPAN_STACK.with_borrow(|stack| stack.last().map(|entry| entry.context()))
}

pub(crate) fn record_error(sqlstate: &str, query: Option<&str>) {
    LAST_ERROR.set(Some(ErrorRecord {
        sqlstate: sqlstate.to_string(),
        query: query.map(|s| s.to_string()),
    }));
}

pub fn define_guc_variables() {
    use pgrx::guc::{GucContext, GucFlags, GucRegistry};

    GucRegistry::define_int_guc(
        c"otel.max_nested_statements",
        c"Maximum nested statement span limit",
        c"Maximum number of nested statement spans that may be emitted across a statement.",
        &GUC_MAX_NESTED_STATEMENTS,
        0,
        i32::MAX / 4,
        GucContext::Suset,
        GucFlags::empty(),
    );

    GucRegistry::define_int_guc(
        c"otel.trace_nested_statements",
        c"Requested nested statement span budget",
        c"Requested number of nested statement spans to emit for procedures and triggers.",
        &GUC_TRACE_NESTED_STATEMENTS,
        0,
        i32::MAX / 4,
        GucContext::Userset,
        GucFlags::empty(),
    );

    unsafe {
        GucRegistry::define_string_guc_with_hooks(
            c"otel.traceparent",
            c"W3C traceparent context",
            c"W3C traceparent header for distributed tracing context propagation.",
            &GUC_TRACEPARENT,
            GucContext::Userset,
            GucFlags::empty(),
            Some(check_traceparent),
            None,
            None,
        );

        GucRegistry::define_string_guc_with_hooks(
            c"otel.tracestate",
            c"W3C tracestate context",
            c"W3C tracestate header for distributed tracing context propagation.",
            &GUC_TRACESTATE,
            GucContext::Userset,
            GucFlags::empty(),
            Some(check_tracestate),
            None,
            None,
        );
    }
}

#[pgrx::pg_guc_hook(check)]
fn check_traceparent(
    value: Option<ffi::CString>,
    source: pg_sys::GucSource::Type,
) -> Result<(), pgrx::guc::GucCheckError> {
    let Some(v) = value else { return Ok(()) };
    let s = v
        .to_str()
        .map_err(|e| pgrx::guc::GucCheckError::new(e.to_string()))?;

    if !s.is_empty() {
        if !IN_GUC_UPDATE.get() {
            if source == pg_sys::GucSource::PGC_S_TEST {
                return Err(pgrx::guc::GucCheckError::new(
                    "otel.traceparent cannot be persisted",
                ));
            }
            if EXECUTION_DEPTH.get() > 0 {
                return Err(pgrx::guc::GucCheckError::new(
                    "cannot set otel.traceparent inside a nested statement",
                ));
            }
        }
        let headers = Headers::new(s, None);
        SpanContext::try_from(headers)
            .map_err(|_| pgrx::guc::GucCheckError::new("invalid traceparent format"))?;
    }
    Ok(())
}

#[pgrx::pg_guc_hook(check)]
fn check_tracestate(
    value: Option<ffi::CString>,
    source: pg_sys::GucSource::Type,
) -> Result<(), pgrx::guc::GucCheckError> {
    let Some(v) = value else { return Ok(()) };
    let s = v
        .to_str()
        .map_err(|e| pgrx::guc::GucCheckError::new(e.to_string()))?;

    if !s.is_empty() && !IN_GUC_UPDATE.get() {
        if source == pg_sys::GucSource::PGC_S_TEST {
            return Err(pgrx::guc::GucCheckError::new(
                "otel.tracestate cannot be persisted",
            ));
        }
        if EXECUTION_DEPTH.get() > 0 {
            return Err(pgrx::guc::GucCheckError::new(
                "cannot set otel.tracestate inside a nested statement",
            ));
        }
    }
    Ok(())
}

fn guc_cstr(guc: &GucString) -> Option<&ffi::CStr> {
    unsafe {
        let v = *guc.as_ptr();
        if v.is_null() || *v == 0 {
            None
        } else {
            Some(ffi::CStr::from_ptr(v))
        }
    }
}

/// Reads W3C trace context from `otel.traceparent` and `otel.tracestate` GUCs without clearing them.
///
/// Used primarily by parallel query workers, where GUC state is restored by PostgreSQL from the
/// leader backend.
fn read_guc_context() -> Option<SpanContext> {
    let tp_str = guc_cstr(&GUC_TRACEPARENT)?.to_str().ok()?;
    let ts_str = guc_cstr(&GUC_TRACESTATE).and_then(|s| s.to_str().ok());
    let headers = Headers::new(tp_str, ts_str);
    SpanContext::try_from(headers).ok()
}

/// Clears the `otel.traceparent` and `otel.tracestate` GUC parameters in session state.
fn clear_guc_context() {
    IN_GUC_UPDATE.set(true);
    unsafe {
        pg_sys::SetConfigOption(
            c"otel.traceparent".as_ptr(),
            c"".as_ptr(),
            pg_sys::GucContext::PGC_USERSET,
            pg_sys::GucSource::PGC_S_SESSION,
        );
        pg_sys::SetConfigOption(
            c"otel.tracestate".as_ptr(),
            c"".as_ptr(),
            pg_sys::GucContext::PGC_USERSET,
            pg_sys::GucSource::PGC_S_SESSION,
        );
    }
    IN_GUC_UPDATE.set(false);
}

/// Reads W3C trace context from session GUCs and resets them to empty strings.
///
/// GUC trace context is single-use and cleared immediately to prevent session pollution
/// across pooled connections.
fn take_guc_context() -> Option<SpanContext> {
    let ctx = read_guc_context();
    if ctx.is_some() || guc_cstr(&GUC_TRACEPARENT).is_some() || guc_cstr(&GUC_TRACESTATE).is_some()
    {
        clear_guc_context();
    }
    ctx
}

/// Extracts incoming trace context for a statement, preferring SQL comments over session GUCs.
fn take_statement_context(
    pstmt: Option<&pg_sys::PlannedStmt>,
    sql: Option<&str>,
) -> Option<SpanContext> {
    // Consume and clear any pending session context GUCs.
    let guc_ctx = take_guc_context();

    let comment_ctx = sql
        .and_then(|s| {
            let (loc, len) = pstmt.map_or((-1, 0), |p| (p.stmt_location, p.stmt_len));
            let body = batch_statement_slice(s, loc, len).unwrap_or(s);

            // Trailing comments (SQLCommenter) take precedence over leading comments.
            //
            // Since PostgreSQL 18, `PlannedStmt::stmt_location` points to the first SQL keyword.
            // Leading comments therefore reside at the tail of the area between the previous
            // statement's terminating delimiter (`BATCH_CURSOR`) and `loc`.
            std::cfg_select! {
                any(feature = "pg15", feature = "pg16", feature = "pg17") => {
                    Headers::from_tail(body).or_else(|| Headers::from_head(body))
                }
                _ => {
                    Headers::from_tail(body).or_else(|| {
                        let cursor = BATCH_CURSOR.get();
                        if loc >= 0 && (loc as usize) > cursor {
                            let prefix = s.get(cursor..loc as usize)?;
                            Headers::from_tail(prefix)
                        } else {
                            None
                        }
                    })
                }
            }
        })
        .and_then(|h| SpanContext::try_from(h).ok());

    comment_ctx.or(guc_ctx)
}

/// Settles the active transaction span's context if it does not yet have any.
///
/// In SQL, `SET LOCAL otel.traceparent` can execute after `BEGIN`. This late-binding step
/// checks if a GUC context arrived after transaction initialization and retroactively
/// updates the transaction span's parent and trace ID.
fn settle_transaction_context() {
    SPAN_STACK.with_borrow_mut(|stack| {
        if let Some(StackSpan::Transaction(xact)) = stack.first_mut()
            && !xact.has_received_context
        {
            if let Some(ctx) = take_guc_context() {
                xact.span.parent_span_id = ctx.span_id.into();
                xact.context.trace_id = ctx.trace_id;
                xact.context.trace_flags = ctx.trace_flags;
                xact.context.trace_state = ctx.trace_state;
            }
            xact.has_received_context = true;
        }
    });
}

/// Initializes trace context and builder for a statement span.
///
/// Automatically settles any pending transaction context before extracting statement context,
/// ensuring proper parenting under active transactions.
fn init_statement_span(
    pstmt: Option<&pg_sys::PlannedStmt>,
    sql: Option<&str>,
) -> (SpanContext, SpanBuilder) {
    settle_transaction_context();
    let incoming = take_statement_context(pstmt, sql);
    propagate_context(incoming, current_span_context())
}

/// Opens a new transaction span for `BEGIN` or `START TRANSACTION` commands.
fn open_transaction_span(
    pstmt: Option<&pg_sys::PlannedStmt>,
    sql: Option<&str>,
    start_time: time::SystemTime,
) -> TransactionSpanState {
    let incoming = take_statement_context(pstmt, sql);
    let has_received_context = incoming.is_some();
    let (context, builder) = propagate_context(incoming, current_span_context());
    let span = builder
        .name("TRANSACTION")
        .start_time(start_time)
        .attributes([
            KeyValue::new_string("db.operation.name", "TRANSACTION"),
            KeyValue::new_string("db.system.name", "postgresql"),
        ])
        .finish();

    TransactionSpanState {
        span,
        context,
        has_received_context,
    }
}

fn export_span(span: &Span) {
    let mut buffer = crate::BytesMut::new();
    if let Err(e) = prost::Message::encode(span, &mut buffer) {
        pgrx::warning!("pg_otel: failed to encode span: {e}");
        return;
    }
    crate::exporter::send_one(&buffer);
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
        let (loc, len) = pstmt.map_or((-1, 0), |p| (p.stmt_location, p.stmt_len));
        let mut text = batch_statement_slice(s, loc, len).unwrap_or(s).trim();
        while !text.is_empty() {
            if text.starts_with("/*")
                && let Some((_, rest)) = text.split_once("*/")
            {
                text = rest.trim();
                continue;
            }
            if text.starts_with("--")
                && let Some((_, rest)) = text.split_once('\n')
            {
                text = rest.trim();
                continue;
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
        && let Some(node) = unsafe { ps.utilityStmt.as_ref() }
    {
        return node.try_as::<pg_sys::VariableSetStmt>().is_some();
    }
    if let Some(s) = sql {
        let (loc, len) = pstmt.map_or((-1, 0), |p| (p.stmt_location, p.stmt_len));
        let mut text = batch_statement_slice(s, loc, len).unwrap_or(s).trim();
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
        && let Some(node) = unsafe { ps.utilityStmt.as_ref() }
    {
        return node
            .try_as::<pg_sys::TransactionStmt>()
            .map(|xact| xact.kind);
    }
    if let Some(s) = sql {
        let (loc, len) = pstmt.map_or((-1, 0), |p| (p.stmt_location, p.stmt_len));
        let mut text = batch_statement_slice(s, loc, len).unwrap_or(s).trim();
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
                unsafe { ps.as_ref() },
                (!qs.is_null()).then(|| unsafe { ffi::CStr::from_ptr(qs) }),
                call_remaining_hooks,
            );
        } else {
            call_remaining_hooks();
        }
    }

    unsafe { pg_sys::RegisterXactCallback(Some(xact_callback), ptr::null_mut()) };
    unsafe { pg_sys::RegisterSubXactCallback(Some(subxact_callback), ptr::null_mut()) };

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
            executor_start(unsafe { qd.as_ref() });
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
            executor_end(unsafe { qd.as_ref() });
        }
    }
}

fn executor_start(query: Option<&pg_sys::QueryDesc>) {
    let now = time::SystemTime::now();
    let Some(qd) = query else { return };

    let depth = EXECUTION_DEPTH.get() + 1;
    EXECUTION_DEPTH.set(depth);

    if unsafe { pg_sys::ParallelWorkerNumber >= 0 } {
        if depth == 1
            && let Some(ctx) = read_guc_context()
        {
            let sql = if !qd.sourceText.is_null() {
                unsafe { ffi::CStr::from_ptr(qd.sourceText).to_str().ok() }
            } else {
                None
            };
            let pstmt = unsafe { qd.plannedstmt.as_ref() };
            let op_name = get_operation_name(pstmt, sql);
            let (context, builder) = start_nested_span(&ctx);
            let span = builder
                .name(op_name.clone())
                .start_time(now)
                .attributes([
                    KeyValue::new_string("db.operation.name", op_name),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ])
                .finish();
            SPAN_STACK.with_borrow_mut(|s| {
                s.push(StackSpan::Statement(ActiveStatement {
                    span,
                    context,
                    depth: 1,
                    scope_budget: Cell::new(0),
                }));
            });
        }
        return;
    }

    if SPAN_STACK.with_borrow(|s| {
        s.last()
            .is_some_and(|entry| entry.is_commit() || entry.is_rollback())
    }) {
        return;
    }

    let pstmt = unsafe { qd.plannedstmt.as_ref() };
    let sql = if !qd.sourceText.is_null() {
        unsafe { ffi::CStr::from_ptr(qd.sourceText).to_str().ok() }
    } else {
        None
    };

    if depth == 1 {
        let initial_budget = GUC_TRACE_NESTED_STATEMENTS
            .get()
            .min(GUC_MAX_NESTED_STATEMENTS.get())
            .max(0);
        REMAINING_BUDGET.set(initial_budget);
        DROPPED_NESTED_STATEMENTS.set(0);

        let (context, builder) = init_statement_span(pstmt, sql);
        let op_name = get_operation_name(pstmt, sql);

        let span = builder
            .name(op_name.clone())
            .start_time(now)
            .attributes([
                KeyValue::new_string("db.operation.name", op_name),
                KeyValue::new_string("db.system.name", "postgresql"),
            ])
            .finish();

        SPAN_STACK.with_borrow_mut(|s| {
            s.push(StackSpan::Statement(ActiveStatement {
                span,
                context,
                depth,
                scope_budget: Cell::new(initial_budget),
            }));
        });
    } else {
        SPAN_STACK.with_borrow_mut(|stack| {
            if let Some(StackSpan::Statement(parent)) = stack.last()
                && parent.scope_budget.get() > 0
                && REMAINING_BUDGET.get() > 0
                && GUC_TRACE_NESTED_STATEMENTS.get() > 0
                && GUC_MAX_NESTED_STATEMENTS.get() > 0
            {
                parent.scope_budget.set(parent.scope_budget.get() - 1);
                REMAINING_BUDGET.set(REMAINING_BUDGET.get() - 1);

                let delegate = parent
                    .scope_budget
                    .get()
                    .min(REMAINING_BUDGET.get())
                    .min(GUC_TRACE_NESTED_STATEMENTS.get())
                    .max(0);

                parent
                    .scope_budget
                    .set(parent.scope_budget.get() - delegate);

                let (context, builder) = start_nested_span(&parent.context);
                let op_name = get_operation_name(pstmt, sql);

                let mut builder = builder.name(op_name.clone()).start_time(now).attributes([
                    KeyValue::new_string("db.operation.name", op_name),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ]);
                if unsafe { pg_sys::pg_trigger_depth(ptr::null_mut()).value() } > 0 {
                    builder = builder.attribute(KeyValue::new_bool("db.postgresql.trigger", true));
                }

                let span = builder.finish();

                stack.push(StackSpan::Statement(ActiveStatement {
                    span,
                    context,
                    depth,
                    scope_budget: Cell::new(delegate),
                }));
            } else {
                DROPPED_NESTED_STATEMENTS.set(DROPPED_NESTED_STATEMENTS.get() + 1);
            }
        });
    }

    if pstmt.is_some_and(|p| p.parallelModeNeeded)
        && let Some(context) = current_span_context()
    {
        let tp = context.w3c_traceparent();
        IN_GUC_UPDATE.set(true);
        unsafe {
            // TODO: Make this nul-terminated without allocating.
            let v = ffi::CString::new(tp).unwrap();
            pg_sys::SetConfigOption(
                c"otel.traceparent".as_ptr(),
                v.as_ptr(),
                pg_sys::GucContext::PGC_USERSET,
                pg_sys::GucSource::PGC_S_SESSION,
            );
            // TODO: Make this nul-terminated without allocating.
            if !context.trace_state.is_empty()
                && let Ok(ts) = ffi::CString::new(&*context.trace_state)
            {
                pg_sys::SetConfigOption(
                    c"otel.tracestate".as_ptr(),
                    ts.as_ptr(),
                    pg_sys::GucContext::PGC_USERSET,
                    pg_sys::GucSource::PGC_S_SESSION,
                );
            }
        }
        IN_GUC_UPDATE.set(false);
    }
}

fn executor_end(query: Option<&pg_sys::QueryDesc>) {
    let depth = EXECUTION_DEPTH.get();
    if depth == 0 {
        return;
    }
    EXECUTION_DEPTH.set(depth - 1);

    if unsafe { pg_sys::ParallelWorkerNumber >= 0 } {
        if depth == 1
            && let Some(entry) = SPAN_STACK.with_borrow_mut(|s| s.pop())
        {
            entry.finish_and_export(time::SystemTime::now());
        }
        return;
    }

    if SPAN_STACK.with_borrow(|s| {
        s.last()
            .is_some_and(|entry| entry.is_commit() || entry.is_rollback())
    }) {
        return;
    }

    if let Some(qd) = query
        && let Some(ps) = unsafe { qd.plannedstmt.as_ref() }
        && ps.parallelModeNeeded
    {
        clear_guc_context();
    }

    if depth == 1
        && let Some(qd) = query
        && let Some(ps) = unsafe { qd.plannedstmt.as_ref() }
        && let Some(sql) = unsafe {
            (!qd.sourceText.is_null()).then(|| ffi::CStr::from_ptr(qd.sourceText).to_str().ok())
        }
        .flatten()
    {
        batch_advance_cursor(sql, ps.stmt_location, ps.stmt_len);
    }

    SPAN_STACK.with_borrow_mut(|s| {
        if s.last().is_some_and(|entry| entry.depth() == depth)
            && let Some(mut entry) = s.pop()
        {
            if depth == 1 {
                let dropped = DROPPED_NESTED_STATEMENTS.get();
                if dropped > 0 {
                    entry.attribute(KeyValue::new_int(
                        "otel.dropped_child_spans",
                        dropped as i64,
                    ));
                }
            } else if let StackSpan::Statement(ref stmt) = entry
                && let Some(StackSpan::Statement(parent)) = s.last()
            {
                parent
                    .scope_budget
                    .set(parent.scope_budget.get() + stmt.scope_budget.get());
            }

            entry.finish_and_export(time::SystemTime::now());
        }
    });
}

fn process_utility<F>(pstmt: Option<&pg_sys::PlannedStmt>, sql: Option<&ffi::CStr>, hooks: F)
where
    F: FnOnce(),
{
    let now = time::SystemTime::now();
    let sql_str = sql.and_then(|s| s.to_str().ok());

    if is_variable_set(pstmt, sql_str) {
        hooks();
        if EXECUTION_DEPTH.get() == 0
            && let Some(ps) = pstmt
            && let Some(src) = sql_str
        {
            batch_advance_cursor(src, ps.stmt_location, ps.stmt_len);
        }
        return;
    }

    let xact_kind = get_transaction_stmt_kind(pstmt, sql_str);
    match xact_kind {
        Some(pg_sys::TransactionStmtKind::TRANS_STMT_BEGIN)
        | Some(pg_sys::TransactionStmtKind::TRANS_STMT_START) => {
            let xact = open_transaction_span(pstmt, sql_str, now);
            SPAN_STACK.with_borrow_mut(|s| {
                s.push(StackSpan::Transaction(xact));
            });

            hooks();
            return;
        }
        Some(pg_sys::TransactionStmtKind::TRANS_STMT_COMMIT) => {
            let (context, builder) = init_statement_span(pstmt, sql_str);
            let span = builder
                .name("COMMIT")
                .start_time(now)
                .attributes([
                    KeyValue::new_string("db.operation.name", "COMMIT"),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ])
                .finish();
            SPAN_STACK.with_borrow_mut(|s| {
                s.push(StackSpan::Statement(ActiveStatement {
                    span,
                    context,
                    depth: EXECUTION_DEPTH.get() + 1,
                    scope_budget: Cell::new(0),
                }));
            });
            hooks();
            return;
        }
        Some(pg_sys::TransactionStmtKind::TRANS_STMT_ROLLBACK) => {
            let (context, builder) = init_statement_span(pstmt, sql_str);
            let span = builder
                .name("ROLLBACK")
                .start_time(now)
                .attributes([
                    KeyValue::new_string("db.operation.name", "ROLLBACK"),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ])
                .finish();
            SPAN_STACK.with_borrow_mut(|s| {
                s.push(StackSpan::Statement(ActiveStatement {
                    span,
                    context,
                    depth: EXECUTION_DEPTH.get() + 1,
                    scope_budget: Cell::new(0),
                }));
            });
            hooks();

            SPAN_STACK.with_borrow_mut(|s| {
                while s
                    .last()
                    .is_some_and(|e| e.is_rollback() || e.is_transaction())
                {
                    if let Some(entry) = s.pop() {
                        entry.finish_and_export(now);
                    }
                }
            });
            return;
        }
        _ => (),
    }

    let depth = EXECUTION_DEPTH.get() + 1;
    EXECUTION_DEPTH.set(depth);

    if depth == 1 {
        let initial_budget = GUC_TRACE_NESTED_STATEMENTS
            .get()
            .min(GUC_MAX_NESTED_STATEMENTS.get())
            .max(0);
        REMAINING_BUDGET.set(initial_budget);
        DROPPED_NESTED_STATEMENTS.set(0);

        let (context, builder) = init_statement_span(pstmt, sql_str);
        let op_name = get_operation_name(pstmt, sql_str);
        let span = builder
            .name(op_name.clone())
            .start_time(now)
            .attributes([
                KeyValue::new_string("db.operation.name", op_name),
                KeyValue::new_string("db.system.name", "postgresql"),
            ])
            .finish();

        SPAN_STACK.with_borrow_mut(|s| {
            s.push(StackSpan::Statement(ActiveStatement {
                span,
                context,
                depth,
                scope_budget: Cell::new(initial_budget),
            }));
        });
    } else {
        SPAN_STACK.with_borrow_mut(|stack| {
            if let Some(StackSpan::Statement(parent)) = stack.last()
                && parent.scope_budget.get() > 0
                && REMAINING_BUDGET.get() > 0
                && GUC_TRACE_NESTED_STATEMENTS.get() > 0
                && GUC_MAX_NESTED_STATEMENTS.get() > 0
            {
                parent.scope_budget.set(parent.scope_budget.get() - 1);
                REMAINING_BUDGET.set(REMAINING_BUDGET.get() - 1);

                let delegate = parent
                    .scope_budget
                    .get()
                    .min(REMAINING_BUDGET.get())
                    .min(GUC_TRACE_NESTED_STATEMENTS.get())
                    .max(0);

                parent
                    .scope_budget
                    .set(parent.scope_budget.get() - delegate);

                let (context, builder) = start_nested_span(&parent.context);
                let op_name = get_operation_name(pstmt, sql_str);

                let mut builder = builder.name(op_name.clone()).start_time(now).attributes([
                    KeyValue::new_string("db.operation.name", op_name),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ]);
                if unsafe { pg_sys::pg_trigger_depth(ptr::null_mut()).value() } > 0 {
                    builder = builder.attribute(KeyValue::new_bool("db.postgresql.trigger", true));
                }

                let span = builder.finish();

                stack.push(StackSpan::Statement(ActiveStatement {
                    span,
                    context,
                    depth,
                    scope_budget: Cell::new(delegate),
                }));
            } else {
                DROPPED_NESTED_STATEMENTS.set(DROPPED_NESTED_STATEMENTS.get() + 1);
            }
        });
    }

    hooks();

    let cur_depth = EXECUTION_DEPTH.get();
    if cur_depth > 0 {
        EXECUTION_DEPTH.set(cur_depth - 1);
    }

    if depth == 1
        && let Some(ps) = pstmt
        && let Some(sql) = sql_str
    {
        batch_advance_cursor(sql, ps.stmt_location, ps.stmt_len);
    }

    SPAN_STACK.with_borrow_mut(|s| {
        if s.last().is_some_and(|entry| entry.depth() == depth)
            && let Some(mut entry) = s.pop()
        {
            if depth == 1 {
                let dropped = DROPPED_NESTED_STATEMENTS.get();
                if dropped > 0 {
                    entry.attribute(KeyValue::new_int(
                        "otel.dropped_child_spans",
                        dropped as i64,
                    ));
                }
            } else if let StackSpan::Statement(ref stmt) = entry
                && let Some(StackSpan::Statement(parent)) = s.last()
            {
                parent
                    .scope_budget
                    .set(parent.scope_budget.get() + stmt.scope_budget.get());
            }

            entry.finish_and_export(time::SystemTime::now());
        }
    });
}

fn abort_statements(
    now: time::SystemTime,
    last_error: Option<ErrorRecord>,
    dropped: usize,
) -> Option<String> {
    let mut statement_entries = Vec::new();
    SPAN_STACK.with_borrow_mut(|stack| {
        while let Some(last) = stack.last() {
            if last.is_statement() {
                statement_entries.push(stack.pop().unwrap());
            } else {
                break;
            }
        }
    });

    if let Some(err_record) = last_error {
        let code = err_record.sqlstate;
        if !statement_entries.is_empty() {
            for mut entry in statement_entries {
                entry.span_mut().status = Some(SpanStatus::error(""));
                entry.attribute(KeyValue::new_string("db.response.status_code", &code));
                if entry.depth() == 1 && dropped > 0 {
                    entry.attribute(KeyValue::new_int(
                        "otel.dropped_child_spans",
                        dropped as i64,
                    ));
                }
                entry.finish_and_export(now);
            }
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
                let op_name = get_operation_name(None, batch_statement_slice(qs, -1, 0));
                if !op_name.is_empty() && op_name != "UNKNOWN" {
                    let (context, builder) = init_statement_span(None, Some(qs));
                    let span = builder
                        .name(op_name.clone())
                        .status(SpanStatus::error(""))
                        .start_time(now)
                        .attributes([
                            KeyValue::new_string("db.operation.name", op_name),
                            KeyValue::new_string("db.system.name", "postgresql"),
                            KeyValue::new_string("db.response.status_code", &code),
                        ])
                        .finish();
                    StackSpan::Statement(ActiveStatement {
                        span,
                        context,
                        depth: 1,
                        scope_budget: Cell::new(0),
                    })
                    .finish_and_export(now);
                }
            }
        }
        Some(code)
    } else {
        for mut entry in statement_entries {
            if entry.depth() == 1 && dropped > 0 {
                entry.attribute(KeyValue::new_int(
                    "otel.dropped_child_spans",
                    dropped as i64,
                ));
            }
            entry.finish_and_export(now);
        }
        None
    }
}

fn transaction_abort() {
    let now = time::SystemTime::now();
    EXECUTION_DEPTH.set(0);
    REMAINING_BUDGET.set(0);
    let dropped = DROPPED_NESTED_STATEMENTS.take();
    let last_error = LAST_ERROR.take();
    let in_xact_block = unsafe { pg_sys::IsTransactionBlock() };
    let is_commit_or_rollback = SPAN_STACK.with_borrow(|s| {
        s.last()
            .is_some_and(|entry| entry.is_commit() || entry.is_rollback())
    });

    let error_code = abort_statements(now, last_error, dropped);

    // Discard any partial batch position when a query fails mid-batch.
    BATCH_CURSOR.set(0);

    SPAN_STACK.with_borrow_mut(|s| {
        if let Some(code) = error_code
            && (is_commit_or_rollback || in_xact_block)
            && let Some(StackSpan::Transaction(xact)) = s.last_mut()
            && xact.span.status.is_none()
        {
            xact.span.status = Some(SpanStatus::error(""));
            xact.span
                .attribute(KeyValue::new_string("db.response.status_code", code));
        }

        if (is_commit_or_rollback || !in_xact_block)
            && s.last().is_some_and(|e| e.is_transaction())
            && let Some(entry) = s.pop()
        {
            entry.finish_and_export(now);
        }
    });
}

fn subxact_abort() {
    let now = time::SystemTime::now();
    EXECUTION_DEPTH.set(0);
    REMAINING_BUDGET.set(0);
    let dropped = DROPPED_NESTED_STATEMENTS.take();
    let last_error = LAST_ERROR.take();

    let _ = abort_statements(now, last_error, dropped);
}

fn transaction_commit() {
    let now = time::SystemTime::now();
    EXECUTION_DEPTH.set(0);
    SPAN_STACK.with_borrow_mut(|s| {
        while s
            .last()
            .is_some_and(|e| e.is_commit() || e.is_transaction())
        {
            if let Some(entry) = s.pop() {
                entry.finish_and_export(now);
            }
        }
    });
}

#[cfg(any(test, feature = "pg_test"))]
mod tests;
