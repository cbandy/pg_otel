// SPDX-License-Identifier: MIT

use crate::otlp::*;
use core::cell::{Cell, RefCell};
use core::ptr;
use pgrx::pg_sys::{self, PgNode};
use std::{ffi, time};

#[cfg(any(test, feature = "pg_test"))]
mod tests;

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
    /// This tracks nested procedural calls, triggers, and statements.
    static STATEMENT_STACK: RefCell<Vec<StatementSpan>> = const { RefCell::new(Vec::new()) };

    /// SQLSTATE code of the most recent aborting error (`ERROR`, `FATAL`, `PANIC`).
    ///
    /// Populated in `emit_log` and consumed when transaction or subtransaction abort callbacks fire.
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

#[derive(Debug)]
struct StatementSpan {
    span: Span,
    depth: usize,
    dropped_children: Cell<usize>,
}

impl StatementSpan {
    fn new(span: Span, depth: usize) -> Self {
        Self {
            span,
            depth,
            dropped_children: Cell::new(0),
        }
    }

    /// Reads the active statement context, parent, and optional transaction link from [`crate::context`]
    /// and returns a pre-populated [`SpanBuilder`], if a statement span is active at the current depth.
    fn build(kind: SpanKind) -> Option<SpanBuilder> {
        let (context, parent_span_id, link) = crate::context::statement()?;
        let mut builder = Span::build()
            .kind(kind as i32)
            .span_id(context.span_id)
            .trace_id(context.trace_id)
            .flags(context.trace_flags)
            .trace_state(&*context.trace_state)
            .parent_span_id(parent_span_id);

        if let Some(tx) = link {
            builder = builder.links([Link::build()
                .span_id(tx.span_id)
                .trace_id(tx.trace_id)
                .flags(tx.trace_flags)
                .trace_state(&*tx.trace_state)
                .finish()]);
        }

        Some(builder)
    }

    /// Initializes and pushes an active statement span onto `STATEMENT_STACK`.
    fn start(pstmt: &pg_sys::PlannedStmt, depth: usize, now: time::SystemTime) {
        let Some(builder) = Self::build(
            if depth == 1 && unsafe { pg_sys::ParallelWorkerNumber < 0 } {
                SpanKind::Server
            } else {
                SpanKind::Internal
            },
        ) else {
            if depth > 1 {
                STATEMENT_STACK.with_borrow_mut(|stack| {
                    if let Some(root) = stack.first() {
                        root.dropped_children.set(root.dropped_children.get() + 1);
                    }
                });
            }
            return;
        };

        let op_name = operation_name(pstmt);
        let mut builder = builder.name(op_name).start_time(now).attributes([
            KeyValue::new_string("db.operation.name", op_name),
            KeyValue::new_string("db.system.name", "postgresql"),
        ]);

        if depth > 1 && unsafe { pg_sys::pg_trigger_depth(ptr::null_mut()).value() } > 0 {
            builder = builder.attribute(KeyValue::new_bool("db.postgresql.trigger", true));
        }

        STATEMENT_STACK.with_borrow_mut(|s| {
            s.push(StatementSpan::new(builder.finish(), depth));
        });
    }

    /// Starts a transaction control statement span (`COMMIT` or `ROLLBACK`).
    fn start_control(name: &'static str, depth: usize, now: time::SystemTime) {
        let Some(builder) = Self::build(SpanKind::Server) else {
            return;
        };

        let span = builder
            .name(name)
            .start_time(now)
            .attributes([
                KeyValue::new_string("db.operation.name", name),
                KeyValue::new_string("db.system.name", "postgresql"),
            ])
            .finish();

        STATEMENT_STACK.with_borrow_mut(|s| {
            s.push(StatementSpan::new(span, depth));
        });
    }

    /// Emits a one-shot error span for a statement that failed before execution.
    fn emit_unstarted_error(sqlstate: &str, now: time::SystemTime) {
        let Some(builder) = Self::build(SpanKind::Server) else {
            return;
        };

        let span = builder
            .name("postgresql")
            .start_time(now)
            .attribute(KeyValue::new_string("db.system.name", "postgresql"))
            .finish();

        StatementSpan::new(span, 1)
            .error(sqlstate)
            .finish_and_export(now);
    }

    /// Pops and exports the matching statement span from `STATEMENT_STACK`.
    fn finish(depth: usize) {
        STATEMENT_STACK.with_borrow_mut(|s| {
            if s.last().is_some_and(|entry| entry.depth == depth)
                && let Some(entry) = s.pop()
            {
                entry.finish_and_export(time::SystemTime::now());
            }
        });
    }

    fn error(mut self, sqlstate: &str) -> Self {
        self.span.status = Some(SpanStatus::error(""));
        self.span
            .attribute(KeyValue::new_string("db.response.status_code", sqlstate));
        self
    }

    fn finish_and_export(mut self, end_time: time::SystemTime) {
        let dropped = self.dropped_children.get();
        if self.depth == 1 && dropped > 0 {
            self.span.attribute(KeyValue::new_int(
                "otel.dropped_child_spans",
                dropped as i64,
            ));
        }
        self.span.end(end_time);
        export_span(&self.span);
    }
}

#[derive(Debug)]
struct TransactionSpan {
    span: Span,
}

impl TransactionSpan {
    /// Starts the current transaction span of this backend process when appropriate.
    fn start(now: time::SystemTime) {
        CURRENT_TRANSACTION.with_borrow_mut(|t| {
            t.get_or_insert_with(|| {
                let span = Span::build()
                    .kind(SpanKind::Server as i32)
                    .name("TRANSACTION")
                    .start_time(now)
                    .attributes([
                        KeyValue::new_string("db.operation.name", "TRANSACTION"),
                        KeyValue::new_string("db.system.name", "postgresql"),
                    ])
                    .finish();

                Self { span }
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

    fn finish_and_export(mut self, end_time: time::SystemTime) {
        let Some((context, parent_span_id)) = crate::context::transaction() else {
            return;
        };

        self.span.parent_span_id = parent_span_id.map(Into::into).unwrap_or_default();
        self.span.span_id = context.span_id.into();
        self.span.trace_id = context.trace_id.into();
        self.span.flags = context.trace_flags.into();
        self.span.trace_state = context.trace_state.to_string();
        self.span.end(end_time);
        export_span(&self.span);
    }
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

    LAST_ERROR.set(Some(sqlstate));
}

fn export_span(span: &Span) {
    let mut buffer = crate::BytesMut::new();
    if let Err(e) = prost::Message::encode(span, &mut buffer) {
        pgrx::warning!("pg_otel: failed to encode span: {e}");
        return;
    }
    crate::exporter::send_one(&buffer);
}

fn operation_name(pstmt: &pg_sys::PlannedStmt) -> &'static str {
    match pstmt.commandType {
        pg_sys::CmdType::CMD_SELECT => "SELECT",
        pg_sys::CmdType::CMD_UPDATE => "UPDATE",
        pg_sys::CmdType::CMD_INSERT => "INSERT",
        pg_sys::CmdType::CMD_DELETE => "DELETE",
        pg_sys::CmdType::CMD_MERGE => "MERGE",
        pg_sys::CmdType::CMD_UTILITY if !pstmt.utilityStmt.is_null() => unsafe {
            ffi::CStr::from_ptr(pg_sys::GetCommandTagName(pg_sys::CreateCommandTag(
                pstmt.utilityStmt,
            )))
        }
        .to_str()
        .unwrap_or("UNKNOWN"),
        _ => "UNKNOWN",
    }
}

/// Called from [`pg_sys::ExecutorStart_hook`] when tracing is enabled.
pub(crate) fn executor_start(pstmt: &pg_sys::PlannedStmt) {
    if crate::context::in_commit_or_rollback() {
        return;
    }

    let now = time::SystemTime::now();
    let depth = crate::context::depth();
    let worker = unsafe { pg_sys::ParallelWorkerNumber >= 0 };

    match (depth, worker) {
        // This might be the beginning of an implicit transaction.
        (1, false) if unsafe { pg_sys::IsTransactionBlock() } => {
            TransactionSpan::start(now);
            StatementSpan::start(pstmt, depth, now);
        }
        (1, true) | (_, false) => StatementSpan::start(pstmt, depth, now),
        (_, true) => {}
    }
}

/// Called from [`pg_sys::ExecutorEnd_hook`] when tracing is enabled.
pub(crate) fn executor_end() {
    if crate::context::in_commit_or_rollback() {
        return;
    }

    let depth = crate::context::depth();
    if depth > 0 {
        StatementSpan::finish(depth);
    }
}

/// Called from [`pg_sys::ProcessUtility_hook`] before downstream hooks when tracing is enabled.
pub(crate) fn process_utility_start(pstmt: &pg_sys::PlannedStmt, tree: &pg_sys::Node) {
    use pg_sys::{NodeTag, TransactionStmtKind as StmtKind};

    let now = time::SystemTime::now();
    let depth = crate::context::depth();

    // Match `NodeTag` and `TransactionStmtKind` rather than `CommandTag` to distinguish between
    // `ROLLBACK` and `ROLLBACK TO`.
    match tree
        .try_as::<pg_sys::TransactionStmt>()
        .map(|xact| xact.kind)
    {
        Some(StmtKind::TRANS_STMT_BEGIN) => TransactionSpan::start(now),
        Some(StmtKind::TRANS_STMT_START) => TransactionSpan::start(now),
        Some(StmtKind::TRANS_STMT_COMMIT) => StatementSpan::start_control("COMMIT", depth, now),
        Some(StmtKind::TRANS_STMT_ROLLBACK) => StatementSpan::start_control("ROLLBACK", depth, now),

        // Do not produce spans for individual SET and RESET statements.
        _ if tree.is_a(NodeTag::T_VariableSetStmt) => {}

        // This might be the beginning of an implicit transaction.
        _ if depth == 1 && unsafe { pg_sys::IsTransactionBlock() } => {
            TransactionSpan::start(now);
            StatementSpan::start(pstmt, depth, now);
        }

        _ => StatementSpan::start(pstmt, depth, now),
    }
}

/// Called from [`pg_sys::ProcessUtility_hook`] after downstream hooks when tracing is enabled.
pub(crate) fn process_utility_end(tree: &pg_sys::Node) {
    use pg_sys::{NodeTag, TransactionStmtKind as StmtKind};

    let depth = crate::context::depth();

    // Match `NodeTag` and `TransactionStmtKind` rather than `CommandTag` to distinguish between
    // `ROLLBACK` and `ROLLBACK TO`.
    match tree
        .try_as::<pg_sys::TransactionStmt>()
        .map(|xact| xact.kind)
    {
        // These start a transaction span that ends at COMMIT or ROLLBACK.
        Some(StmtKind::TRANS_STMT_BEGIN | StmtKind::TRANS_STMT_START) => {}

        // Postgres handles COMMIT and ROLLBACK differently when the transaction is already
        // aborted because of an earlier error.
        //
        // When the transaction is still active, Postgres does the work of COMMIT and ROLLBACK
        // after `ProcessUtility`, finished in `transaction_commit()` or `transaction_abort()`.
        // When it is already aborted, Postgres will not fire those callbacks again, so we
        // finalize the statement and transaction spans immediately here.
        Some(StmtKind::TRANS_STMT_COMMIT | StmtKind::TRANS_STMT_ROLLBACK) => {
            if !unsafe { pg_sys::IsTransactionState() } {
                TransactionSpan::finish(time::SystemTime::now());
            }
        }

        // No spans for individual SET and RESET statements.
        _ if tree.is_a(NodeTag::T_VariableSetStmt) => {}

        _ => StatementSpan::finish(depth),
    }
}

fn abort_statements(now: time::SystemTime, error_code: Option<&str>) {
    // Take the entire stack while retaining the allocated capacity.
    #[allow(clippy::drain_collect)]
    let statement_entries: Vec<StatementSpan> =
        STATEMENT_STACK.with_borrow_mut(|stack| stack.drain(..).collect());

    match (statement_entries.is_empty(), error_code) {
        (true, Some(code)) => StatementSpan::emit_unstarted_error(code, now),
        (false, Some(code)) => {
            for entry in statement_entries {
                entry.error(code).finish_and_export(now);
            }
        }
        (false, None) => {
            for entry in statement_entries {
                entry.finish_and_export(now);
            }
        }
        (true, None) => (),
    }
}

/// Called unconditionally via [`pg_sys::RegisterXactCallback`] on transaction abort.
///
/// Drains all active statement spans, marks them with error status, and finalizes the transaction span.
pub(crate) fn transaction_abort() {
    let now = time::SystemTime::now();
    let error_code = LAST_ERROR.take();
    abort_statements(now, error_code.as_deref());

    CURRENT_TRANSACTION.with_borrow_mut(|t| {
        if let Some(code) = error_code
            && let Some(xact) = t.as_mut()
            && xact.span.status.is_none()
        {
            xact.span.status = Some(SpanStatus::error(""));
            xact.span
                .attribute(KeyValue::new_string("db.response.status_code", code));
        }

        if !crate::context::is_transaction_retained_on_abort()
            && let Some(xact) = t.take()
        {
            xact.finish_and_export(now);
        }
    });
}

/// Called unconditionally via [`pg_sys::RegisterSubXactCallback`] on subtransaction abort.
///
/// Drains active statement spans and clears `LAST_ERROR`.
pub(crate) fn subtransaction_abort() {
    let now = time::SystemTime::now();
    let error_code = LAST_ERROR.take();
    abort_statements(now, error_code.as_deref());
}

/// Called unconditionally via [`pg_sys::RegisterXactCallback`] on transaction commit.
///
/// Finalizes and exports any remaining statement spans and the transaction span.
pub(crate) fn transaction_commit() {
    let now = time::SystemTime::now();
    TransactionSpan::finish(now);
}
