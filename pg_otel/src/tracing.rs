// SPDX-License-Identifier: MIT

use crate::context::{Headers, SpanContext};
use crate::otlp::*;
use core::cell::{Cell, RefCell};
use core::ptr;
use pgrx::pg_sys::{self, PgNode};
use std::{ffi, time};

#[cfg(any(test, feature = "pg_test"))]
mod tests;

thread_local! {
    /// Tracks the starting byte offset of the unparsed remainder in a multi-statement simple query batch.
    ///
    /// Since PostgreSQL 18, `PlannedStmt::stmt_location` advances past leading comments to the first SQL keyword.
    /// The value here marks where the previous statement ended so leading comments can be recovered without backward scanning.
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

thread_local! {
    /// The current top-level transaction span, if any, of this backend process.
    ///
    /// Backend processes run at most one active transaction at a time.
    /// This is populated (1) at the beginning of an explicit transaction (`BEGIN`,`START`)
    /// or (2) when processing the first statement of a simple-query batch (implicit transaction).
    /// It is cleared during transaction commit or abort.
    static CURRENT_TRANSACTION: RefCell<Option<TransactionSpan>> = const { RefCell::new(None) };

    /// LIFO stack of actively executing statements in this backend process.
    ///
    /// Entries are pushed when statement execution begins and popped upon completion.
    /// This tracks nested procedural calls, triggers, and statements, preserving
    /// the parent-child span hierarchy and allocated nesting budgets.
    static STATEMENT_STACK: RefCell<Vec<StatementSpan>> = const { RefCell::new(Vec::new()) };
}

#[derive(Debug)]
struct StatementSpan {
    span: Span,
    context: SpanContext,
    depth: usize,
    scope_budget: Cell<u32>,
    dropped_children: Cell<usize>,
}

impl StatementSpan {
    fn new(span: Span, context: SpanContext, depth: usize, scope_budget: u32) -> Self {
        Self {
            span,
            context,
            depth,
            scope_budget: Cell::new(scope_budget),
            dropped_children: Cell::new(0),
        }
    }

    /// Derives trace context and provides a builder for a statement span.
    ///
    /// Binds any pending transaction context before extracting statement context,
    /// ensuring proper parenting under active transactions.
    fn derive_context(
        pstmt: Option<&pg_sys::PlannedStmt>,
        sql: Option<&str>,
    ) -> (SpanContext, SpanBuilder) {
        let parent = STATEMENT_STACK
            .with_borrow(|s| s.last().map(|entry| entry.context.clone()))
            .or_else(|| {
                CURRENT_TRANSACTION.with_borrow_mut(|t| t.as_mut().map(|xact| xact.bind()))
            });
        let incoming = take_statement_context(pstmt, sql);
        let builder = Span::build().kind(SpanKind::Server as i32);

        match (incoming, parent) {
            (None, None) => (SpanContext::root(), builder),
            (Some(i), None) => (
                SpanContext::child_of(&i),
                builder.parent_span_id(Some(i.span_id)),
            ),
            (None, Some(p)) => (
                SpanContext::child_of(&p),
                builder.parent_span_id(Some(p.span_id)),
            ),
            (Some(i), Some(p)) => (
                SpanContext::child_of(&i),
                builder.parent_span_id(Some(i.span_id)).links([Link::build()
                    .span_id(p.span_id)
                    .trace_id(p.trace_id)
                    .flags(p.trace_flags)
                    .trace_state(&*p.trace_state)
                    .finish()]),
            ),
        }
    }

    /// Initializes and pushes an active statement span onto `STATEMENT_STACK`.
    ///
    /// Handles top-level statement spans: implicit transactions, nested statement budget,
    /// and trigger detection.
    fn start(
        pstmt: Option<&pg_sys::PlannedStmt>,
        sql: Option<&str>,
        depth: usize,
        now: time::SystemTime,
    ) {
        if depth == 1 {
            TransactionSpan::start(None, now);

            let initial_budget = crate::config::nested_statements_request()
                .min(crate::config::nested_statements_max());
            REMAINING_BUDGET.set(initial_budget);

            let (context, builder) = Self::derive_context(pstmt, sql);
            let op_name = get_operation_name(pstmt, sql);

            let span = builder
                .name(op_name.clone())
                .start_time(now)
                .attributes([
                    KeyValue::new_string("db.operation.name", op_name),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ])
                .finish();

            STATEMENT_STACK.with_borrow_mut(|s| {
                s.push(StatementSpan::new(span, context, depth, initial_budget));
            });
        } else {
            STATEMENT_STACK.with_borrow_mut(|stack| {
                if let Some(parent) = stack.last()
                    && parent.scope_budget.get() > 0
                    && REMAINING_BUDGET.get() > 0
                    && crate::config::nested_statements_request() > 0
                    && crate::config::nested_statements_max() > 0
                {
                    parent.scope_budget.set(parent.scope_budget.get() - 1);
                    REMAINING_BUDGET.set(REMAINING_BUDGET.get() - 1);

                    let delegate = parent
                        .scope_budget
                        .get()
                        .min(REMAINING_BUDGET.get())
                        .min(crate::config::nested_statements_request());

                    parent
                        .scope_budget
                        .set(parent.scope_budget.get() - delegate);

                    let context = SpanContext::child_of(&parent.context);
                    let op_name = get_operation_name(pstmt, sql);

                    let mut builder = Span::build()
                        .parent_span_id(Some(parent.context.span_id))
                        .kind(SpanKind::Internal as i32)
                        .name(op_name.clone())
                        .start_time(now)
                        .attributes([
                            KeyValue::new_string("db.operation.name", op_name),
                            KeyValue::new_string("db.system.name", "postgresql"),
                        ]);
                    if unsafe { pg_sys::pg_trigger_depth(ptr::null_mut()).value() } > 0 {
                        builder =
                            builder.attribute(KeyValue::new_bool("db.postgresql.trigger", true));
                    }

                    let span = builder.finish();

                    stack.push(StatementSpan::new(span, context, depth, delegate));
                } else if let Some(root) = stack.first() {
                    root.dropped_children.set(root.dropped_children.get() + 1);
                }
            });
        }
    }

    /// Starts a transaction control statement span (`COMMIT` or `ROLLBACK`) with zero budget.
    fn start_control(
        name: &'static str,
        pstmt: Option<&pg_sys::PlannedStmt>,
        sql: Option<&str>,
        depth: usize,
        now: time::SystemTime,
    ) {
        let (context, builder) = Self::derive_context(pstmt, sql);
        let span = builder
            .name(name)
            .start_time(now)
            .attributes([
                KeyValue::new_string("db.operation.name", name),
                KeyValue::new_string("db.system.name", "postgresql"),
            ])
            .finish();

        STATEMENT_STACK.with_borrow_mut(|s| {
            s.push(StatementSpan::new(span, context, depth, 0));
        });
    }

    /// Starts a parallel worker statement span inheriting context from the parent process.
    fn start_parallel_worker(
        parent_ctx: &SpanContext,
        pstmt: Option<&pg_sys::PlannedStmt>,
        sql: Option<&str>,
        now: time::SystemTime,
    ) {
        let op_name = get_operation_name(pstmt, sql);
        let context = SpanContext::child_of(parent_ctx);
        let span = Span::build()
            .parent_span_id(Some(parent_ctx.span_id))
            .kind(SpanKind::Internal as i32)
            .name(op_name.clone())
            .start_time(now)
            .attributes([
                KeyValue::new_string("db.operation.name", op_name),
                KeyValue::new_string("db.system.name", "postgresql"),
            ])
            .finish();

        STATEMENT_STACK.with_borrow_mut(|s| {
            s.push(StatementSpan::new(span, context, 1, 0));
        });
    }

    /// Emits a one-shot error span for a statement that failed before execution.
    fn emit_unstarted_error(sqlstate: &str, query: &str, now: time::SystemTime) {
        let op_name = get_operation_name(None, Some(query));
        if !op_name.is_empty() && op_name != "UNKNOWN" {
            let (context, builder) = Self::derive_context(None, Some(query));
            let span = builder
                .name(op_name.clone())
                .status(SpanStatus::error(""))
                .start_time(now)
                .attributes([
                    KeyValue::new_string("db.operation.name", op_name),
                    KeyValue::new_string("db.system.name", "postgresql"),
                    KeyValue::new_string("db.response.status_code", sqlstate),
                ])
                .finish();

            StatementSpan::new(span, context, 1, 0).finish_and_export(now);
        }
    }

    /// Pops and exports the matching statement span from `STATEMENT_STACK`.
    fn finish(depth: usize) {
        STATEMENT_STACK.with_borrow_mut(|s| {
            if s.last().is_some_and(|entry| entry.depth == depth)
                && let Some(entry) = s.pop()
            {
                if let Some(parent) = s.last() {
                    parent
                        .scope_budget
                        .set(parent.scope_budget.get() + entry.scope_budget.get());
                }

                entry.finish_and_export(time::SystemTime::now());
            }
        });
    }

    fn is_commit(&self) -> bool {
        self.span.name == "COMMIT"
    }

    fn is_rollback(&self) -> bool {
        self.span.name == "ROLLBACK"
    }

    fn attribute(&mut self, kv: KeyValue) {
        self.span.attribute(kv);
    }

    fn finish_and_export(mut self, end_time: time::SystemTime) {
        let dropped = self.dropped_children.get();
        if self.depth == 1 && dropped > 0 {
            self.span.attribute(KeyValue::new_int(
                "otel.dropped_child_spans",
                dropped as i64,
            ));
        }
        self.span.span_id = self.context.span_id.into();
        self.span.trace_id = self.context.trace_id.into();
        self.span.flags = self.context.trace_flags.into();
        self.span.trace_state = self.context.trace_state.to_string();
        self.span.end(end_time);
        export_span(&self.span);
    }
}

#[derive(Debug)]
struct TransactionSpan {
    span: Span,
    context: Option<SpanContext>,
    is_explicit: bool,
}

impl TransactionSpan {
    /// Starts the current transaction span of this backend process when appropriate.
    ///
    /// When `explicit` is `Some`, an existing transaction is marked explicit, or an explicit
    /// transaction is started. When `explicit` is `None`, an implicit transaction is started
    /// when the backend indicates it is inside a multi-statement simple query batch.
    fn start(
        explicit: Option<(Option<&pg_sys::PlannedStmt>, Option<&str>)>,
        now: time::SystemTime,
    ) {
        CURRENT_TRANSACTION.with_borrow_mut(|t| {
            let is_explicit = explicit.is_some();

            // When called explicitly, mark an existing transaction as explicit.
            if let Some(xact) = t.as_mut() {
                xact.is_explicit |= is_explicit;
                return;
            }

            // Only start an implicit transaction when inside a batch transaction.
            if !is_explicit && !unsafe { pg_sys::IsTransactionBlock() } {
                return;
            }

            let incoming = explicit.and_then(|(pstmt, sql)| take_statement_context(pstmt, sql));
            let context = incoming.as_ref().map(SpanContext::child_of);
            let span = Span::build()
                .parent_span_id(incoming.map(|i| i.span_id))
                .kind(SpanKind::Server as i32)
                .name("TRANSACTION")
                .start_time(now)
                .attributes([
                    KeyValue::new_string("db.operation.name", "TRANSACTION"),
                    KeyValue::new_string("db.system.name", "postgresql"),
                ])
                .finish();

            *t = Some(Self {
                span,
                context,
                is_explicit,
            });
        });
    }

    /// Finishes and exports the current transaction and any remaining statements on the stack.
    fn finish(now: time::SystemTime) {
        STATEMENT_STACK.with_borrow_mut(|s| {
            while let Some(entry) = s.pop() {
                entry.finish_and_export(now);
            }
        });
        if let Some(xact) = CURRENT_TRANSACTION.with_borrow_mut(|t| t.take()) {
            xact.finish_and_export(now);
        }
    }

    /// Binds this span's context if it is not yet bound.
    ///
    /// In SQL, `SET LOCAL otel.traceparent` can execute after `BEGIN`. This late-binding step
    /// checks if a GUC context arrived after transaction initialization and retroactively
    /// updates this span's parent and trace ID.
    fn bind(&mut self) -> SpanContext {
        if let Some(ctx) = &self.context {
            return ctx.clone();
        }

        let ctx = if let Some(incoming) = crate::config::take_trace_context() {
            self.span.parent_span_id = incoming.span_id.into();
            SpanContext::child_of(&incoming)
        } else {
            SpanContext::root()
        };

        self.context = Some(ctx.clone());
        ctx
    }

    fn finish_and_export(mut self, end_time: time::SystemTime) {
        let context = self.bind();
        self.span.span_id = context.span_id.into();
        self.span.trace_id = context.trace_id.into();
        self.span.flags = context.trace_flags.into();
        self.span.trace_state = context.trace_state.to_string();
        self.span.end(end_time);
        export_span(&self.span);
    }
}

struct ErrorRecord {
    sqlstate: String,
    query: Option<String>,
}

thread_local! {
    static EXECUTION_DEPTH: Cell<usize> = const { Cell::new(0) };
    static REMAINING_BUDGET: Cell<u32> = const { Cell::new(0) };
    static LAST_ERROR: RefCell<Option<ErrorRecord>> = const { RefCell::new(None) };
}

pub(crate) fn execution_depth() -> usize {
    EXECUTION_DEPTH.get()
}

pub(crate) fn current_span_context() -> Option<SpanContext> {
    STATEMENT_STACK
        .with_borrow(|s| s.last().map(|entry| entry.context.clone()))
        .or_else(|| {
            CURRENT_TRANSACTION.with_borrow(|t| t.as_ref().and_then(|xact| xact.context.clone()))
        })
}

/// Called from [`pg_sys::emit_log_hook`] when tracing is enabled.
pub(crate) fn emit_log(record: &pg_sys::ErrorData) {
    use pg_sys::errcodes::PgSqlErrorCode;

    // Capture errors that abort execution (ERROR, FATAL, PANIC) and carry a valid SQLSTATE.
    // This information is needed later, when Postgres calls transaction abort callbacks.
    if record.elevel < pg_sys::PGERROR as i32
        || record.sqlerrcode == PgSqlErrorCode::ERRCODE_SUCCESSFUL_COMPLETION as i32
    {
        return;
    }

    // `unpack_sql_state` returns a pointer to a static buffer; make a copy here.
    let unpacked = unsafe { pg_sys::unpack_sql_state(record.sqlerrcode) };
    let Ok(sqlstate) = (unsafe { ffi::CStr::from_ptr(unpacked) })
        .to_str()
        .map(str::to_owned)
    else {
        return;
    };

    // Make a copy of `debug_query_string` when present.
    let query = unsafe { pg_sys::debug_query_string };
    let query = (!query.is_null())
        .then(|| unsafe { ffi::CStr::from_ptr(query).to_str().ok() })
        .flatten()
        .map(str::to_owned);

    LAST_ERROR.set(Some(ErrorRecord { sqlstate, query }));
}

/// Extracts incoming trace context for a statement, preferring SQL comments over session GUCs.
fn take_statement_context(
    pstmt: Option<&pg_sys::PlannedStmt>,
    sql: Option<&str>,
) -> Option<SpanContext> {
    // Consume and clear any pending session context GUCs.
    let guc_ctx = crate::config::take_trace_context();

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

/// Called from [`pg_sys::ExecutorStart_hook`] when tracing is enabled.
pub(crate) fn executor_start(pstmt: &pg_sys::PlannedStmt, sql: Option<&str>) {
    let now = time::SystemTime::now();
    let depth = EXECUTION_DEPTH.get() + 1;
    EXECUTION_DEPTH.set(depth);

    if unsafe { pg_sys::ParallelWorkerNumber >= 0 } {
        if depth == 1
            && let Some(ctx) = crate::config::read_trace_context()
        {
            StatementSpan::start_parallel_worker(&ctx, Some(pstmt), sql, now);
        }
        return;
    }

    if STATEMENT_STACK.with_borrow(|s| {
        s.last()
            .is_some_and(|entry| entry.is_commit() || entry.is_rollback())
    }) {
        return;
    }

    StatementSpan::start(Some(pstmt), sql, depth, now);

    if pstmt.parallelModeNeeded
        && let Some(context) = current_span_context()
    {
        crate::config::set_trace_context(&context.w3c_traceparent(), &context.trace_state);
    }
}

/// Called from [`pg_sys::ExecutorEnd_hook`] when tracing is enabled.
pub(crate) fn executor_end(pstmt: &pg_sys::PlannedStmt, sql: Option<&str>) {
    let depth = EXECUTION_DEPTH.get();
    if depth == 0 {
        return;
    }
    EXECUTION_DEPTH.set(depth - 1);

    if unsafe { pg_sys::ParallelWorkerNumber >= 0 } {
        if depth == 1
            && let Some(entry) = STATEMENT_STACK.with_borrow_mut(|s| s.pop())
        {
            entry.finish_and_export(time::SystemTime::now());
        }
        return;
    }

    if STATEMENT_STACK.with_borrow(|s| {
        s.last()
            .is_some_and(|entry| entry.is_commit() || entry.is_rollback())
    }) {
        return;
    }

    if pstmt.parallelModeNeeded {
        crate::config::clear_trace_context();
    }

    if depth == 1
        && let Some(sql) = sql
    {
        batch_advance_cursor(sql, pstmt.stmt_location, pstmt.stmt_len);
    }

    StatementSpan::finish(depth);
}

/// Called from [`pg_sys::ProcessUtility_hook`] when tracing is enabled.
pub(crate) fn process_utility<F>(
    pstmt: &pg_sys::PlannedStmt,
    tree: &pg_sys::Node,
    sql: Option<&str>,
    hooks: F,
) where
    F: FnOnce(),
{
    use pg_sys::{NodeTag, TransactionStmtKind as StmtKind};

    let now = time::SystemTime::now();
    let is_top_level = EXECUTION_DEPTH.get() == 0;
    let depth = EXECUTION_DEPTH.get() + 1;
    EXECUTION_DEPTH.set(depth);

    match tree
        .try_as::<pg_sys::TransactionStmt>()
        .map(|xact| xact.kind)
    {
        Some(StmtKind::TRANS_STMT_BEGIN | StmtKind::TRANS_STMT_START) => {
            TransactionSpan::start(Some((Some(pstmt), sql)), now);

            hooks();
        }
        Some(kind @ (StmtKind::TRANS_STMT_COMMIT | StmtKind::TRANS_STMT_ROLLBACK)) => {
            if kind == StmtKind::TRANS_STMT_COMMIT {
                StatementSpan::start_control("COMMIT", Some(pstmt), sql, depth, now);
            } else {
                StatementSpan::start_control("ROLLBACK", Some(pstmt), sql, depth, now);
            }

            // Postgres handles COMMIT and ROLLBACK differently when the transaction is already
            // aborted because of an earlier error.
            let txn_aborted = unsafe { pg_sys::IsAbortedTransactionBlockState() };

            hooks();

            // When the transaction *is not* aborted, Postgres does the work of COMMIT and ROLLBACK
            // *after* `process_utility()`. In that case, we can measure that work by finishing the
            // span in `transaction_commit()` and `transaction_abort()`.
            //
            // When the transaction *is* aborted, Postgres has already called `transaction_abort()`
            // and won't call it again. We finish the statement span immediately and emit the
            // transaction here.
            if txn_aborted {
                TransactionSpan::finish(now);
            }
        }
        // Do not produce spans for individual SET and RESET statements.
        _ if tree.is_a(NodeTag::T_VariableSetStmt) => {
            hooks();
        }
        _ => {
            StatementSpan::start(Some(pstmt), sql, depth, now);

            hooks();

            StatementSpan::finish(depth);
        }
    }

    let cur_depth = EXECUTION_DEPTH.get();
    if cur_depth > 0 {
        EXECUTION_DEPTH.set(cur_depth - 1);
    }

    if is_top_level && let Some(src) = sql {
        batch_advance_cursor(src, pstmt.stmt_location, pstmt.stmt_len);
    }
}

fn abort_statements(now: time::SystemTime, last_error: Option<ErrorRecord>) -> Option<String> {
    // Take the entire stack while retaining the allocated capacity.
    #[allow(clippy::drain_collect)]
    let statement_entries: Vec<StatementSpan> =
        STATEMENT_STACK.with_borrow_mut(|stack| stack.drain(..).collect());

    if let Some(err_record) = last_error {
        let code = err_record.sqlstate;
        if !statement_entries.is_empty() {
            for mut entry in statement_entries {
                entry.span.status = Some(SpanStatus::error(""));
                entry.attribute(KeyValue::new_string("db.response.status_code", &code));
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
                StatementSpan::emit_unstarted_error(&code, qs, now);
            }
        }
        Some(code)
    } else {
        for entry in statement_entries {
            entry.finish_and_export(now);
        }
        None
    }
}

/// Called unconditionally via [`pg_sys::RegisterXactCallback`] on transaction abort.
///
/// Drains all active statement spans, marks them with error status, finalizes the transaction span,
/// and resets `EXECUTION_DEPTH` to 0.
pub(crate) fn transaction_abort() {
    let now = time::SystemTime::now();
    EXECUTION_DEPTH.set(0);
    REMAINING_BUDGET.set(0);
    let last_error = LAST_ERROR.take();
    let in_xact_block = unsafe { pg_sys::IsTransactionBlock() };

    // Peek the stack before abort_statements() drains it.
    // For intermediate statement errors (e.g. SELECT 1/0), CURRENT_TRANSACTION is
    // retained in memory awaiting ROLLBACK. However, if the abort was triggered by
    // COMMIT (e.g. deferred constraint failure) or clean ROLLBACK, the transaction
    // block is terminating and CURRENT_TRANSACTION must be exported now.
    let is_commit_or_rollback = STATEMENT_STACK.with_borrow(|s| {
        s.last()
            .is_some_and(|entry| entry.is_commit() || entry.is_rollback())
    });

    let error_code = abort_statements(now, last_error);

    // Discard any partial batch position when a query fails mid-batch.
    BATCH_CURSOR.set(0);

    let xact_to_export = CURRENT_TRANSACTION.with_borrow_mut(|t| {
        if let Some(code) = error_code
            && let Some(xact) = t.as_mut()
            && xact.span.status.is_none()
        {
            xact.span.status = Some(SpanStatus::error(""));
            xact.span
                .attribute(KeyValue::new_string("db.response.status_code", code));
        }

        let is_explicit = t.as_ref().is_some_and(|xact| xact.is_explicit);

        if is_explicit && in_xact_block && !is_commit_or_rollback {
            None
        } else {
            t.take()
        }
    });

    if let Some(xact) = xact_to_export {
        xact.finish_and_export(now);
    }
}

/// Called unconditionally via [`pg_sys::RegisterSubXactCallback`] on subtransaction abort.
///
/// Drains active statement spans, clears `LAST_ERROR`, and resets `EXECUTION_DEPTH` to 0.
pub(crate) fn subtransaction_abort() {
    let now = time::SystemTime::now();
    let last_error = LAST_ERROR.take();
    EXECUTION_DEPTH.set(0);
    REMAINING_BUDGET.set(0);

    let _ = abort_statements(now, last_error);
}

/// Called unconditionally via [`pg_sys::RegisterXactCallback`] on transaction commit.
///
/// Finalizes and exports any remaining statement spans and the transaction span, then resets
/// `EXECUTION_DEPTH` to 0.
pub(crate) fn transaction_commit() {
    let now = time::SystemTime::now();
    EXECUTION_DEPTH.set(0);
    TransactionSpan::finish(now);
}
