// SPDX-License-Identifier: MIT

mod sqlcommenter;

use self::sqlcommenter::CommenterIterator;
use crate::Str;
use core::cell::{Cell, RefCell};
use pgrx::pg_sys::{self, PgNode};
use pgrx::void_mut_ptr;
use std::ffi;
use std::rc::Rc;

thread_local! {
    static PRNG: Cell<PseudoRandom> = Cell::default();
}

/// Random number generator that uses Postgres [`pg_sys::pg_prng_uint64`].
#[derive(Clone, Copy, Default)]
struct PseudoRandom {
    state: pg_sys::pg_prng_state,
    pid: pg_sys::pid_t,
}

impl PseudoRandom {
    // Seed the pseudorandom state after fork(2) or when uninitialized.
    fn ensure_seeded(&mut self) {
        let pid = unsafe { pg_sys::MyProcPid };

        if self.pid != pid || self.pid == 0 {
            let mut seed = [0u8; 8];
            unsafe {
                pg_sys::pg_strong_random(seed.as_mut_ptr() as void_mut_ptr, 8);
                pg_sys::pg_prng_seed(&mut self.state, u64::from_ne_bytes(seed));
            }
            self.pid = pid;
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.ensure_seeded();

        unsafe { pg_sys::pg_prng_uint64(&mut self.state) }
    }

    fn next_u128(&mut self) -> u128 {
        self.ensure_seeded();

        let hi = unsafe { pg_sys::pg_prng_uint64(&mut self.state) } as u128;
        let lo = unsafe { pg_sys::pg_prng_uint64(&mut self.state) } as u128;

        (hi << 64) | lo
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SpanId(u64);

impl SpanId {
    // Generate a new (uniformly pseudorandom) span identifier.
    pub fn random() -> Self {
        PRNG.with(|cell| {
            let mut prng = cell.get();

            // Repeat until the value is nonzero; W3C Trace Context § 3.2.2.4
            loop {
                let id = prng.next_u64();
                if id != 0 {
                    cell.set(prng);
                    return Self(id);
                }
            }
        })
    }

    pub fn from_hex(hex: &str) -> Result<Self, ()> {
        match hex.len() {
            16 => u64::from_str_radix(hex, 16).map(Self).map_err(|_| ()),
            _ => Err(()),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.0 != 0
    }
}

impl From<SpanId> for Vec<u8> {
    fn from(id: SpanId) -> Self {
        id.0.to_be_bytes().to_vec()
    }
}

impl core::fmt::LowerHex for SpanId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl core::fmt::Debug for SpanId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "SpanId({:016x})", self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TraceId(u128);

impl TraceId {
    // Generate a new (uniformly pseudorandom) trace identifier.
    pub fn random() -> Self {
        PRNG.with(|cell| {
            let mut prng = cell.get();

            // Repeat until the value is nonzero; W3C Trace Context § 3.2.2.3
            loop {
                let id = prng.next_u128();
                if id != 0 {
                    cell.set(prng);
                    return Self(id);
                }
            }
        })
    }

    pub fn from_hex(hex: &str) -> Result<Self, ()> {
        match hex.len() {
            32 => u128::from_str_radix(hex, 16).map(Self).map_err(|_| ()),
            _ => Err(()),
        }
    }

    pub fn is_valid(&self) -> bool {
        self.0 != 0
    }
}

impl core::convert::From<TraceId> for Vec<u8> {
    fn from(id: TraceId) -> Self {
        id.0.to_be_bytes().to_vec()
    }
}

impl core::fmt::LowerHex for TraceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl core::fmt::Debug for TraceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "TraceId({:032x})", self.0)
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct TraceFlags(u8);

impl TraceFlags {
    pub fn from_hex(hex: &str) -> Result<Self, ()> {
        match hex.len() {
            2 => u8::from_str_radix(hex, 16).map(Self).map_err(|_| ()),
            _ => Err(()),
        }
    }

    pub const W3C_RANDOM: Self = Self(0x02); // Indicates the right-most bytes are uniformly random; W3C Trace Context § 3.2.2.5
    pub const W3C_SAMPLED: Self = Self(0x01); // Indicates the caller may have recorded trace data; W3C Trace Context § 3.2.2.5

    pub fn is_random(self) -> bool {
        self.0 & 0x02 != 0
    }

    pub fn is_sampled(self) -> bool {
        self.0 & 0x01 != 0
    }
}

impl core::ops::BitOr for TraceFlags {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl core::convert::From<TraceFlags> for u32 {
    #[inline]
    fn from(f: TraceFlags) -> Self {
        f.0 as u32
    }
}

impl core::fmt::LowerHex for TraceFlags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:02x}", self.0)
    }
}

impl core::fmt::Debug for TraceFlags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "TraceFlags({:02x})", self.0)
    }
}

/// W3C Trace Context headers extracted from a statement or session.
#[derive(Debug, PartialEq)]
pub struct Headers<'a> {
    /// Required [W3C `traceparent` header](https://www.w3.org/TR/trace-context#traceparent-header)
    pub traceparent: Str<'a>,
    /// Optional [W3C `tracestate` header](https://www.w3.org/TR/trace-context#tracestate-header)
    pub tracestate: Option<Str<'a>>,
}

/// Context identifying a span within an OpenTelemetry trace.
///
/// # Performance
/// Cloning a `SpanContext` is $O(1)$ with zero allocations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpanContext {
    pub span_id: SpanId,
    pub trace_id: TraceId,
    pub trace_flags: TraceFlags,
    pub trace_state: Rc<str>,
}

impl SpanContext {
    /// Derives a child span context from a parent, inheriting the trace ID, trace flags,
    /// and trace state, while generating a new random span ID.
    pub fn child_of(parent: &Self) -> Self {
        Self {
            span_id: SpanId::random(),
            trace_id: parent.trace_id,
            trace_flags: parent.trace_flags,
            trace_state: parent.trace_state.clone(),
        }
    }

    /// Generates a new root trace context with a random trace ID, random span ID,
    /// and standard random sampled trace flags (`0x03`).
    pub fn root() -> Self {
        Self {
            span_id: SpanId::random(),
            trace_id: TraceId::random(),
            trace_flags: TraceFlags::W3C_RANDOM | TraceFlags::W3C_SAMPLED,
            trace_state: Rc::default(),
        }
    }

    // Representation defined by W3C Trace Context, Traceparent § 3.2
    pub fn w3c_traceparent(&self) -> String {
        format!(
            "00-{:032x}-{:016x}-{:02x}",
            self.trace_id, self.span_id, self.trace_flags
        )
    }
}

/// Parses a [`SpanContext`] from W3C [`Headers`].
///
/// # W3C Trace Context Specification
///
/// Implements parsing of the `traceparent` and `tracestate` headers according to [W3C Trace Context](https://www.w3.org/TR/trace-context):
///
/// - **`traceparent` format**: Expects version `00` followed by 32-hex of `trace-id`, 16-hex of `parent-id`, and 2-hex of `trace-flags` separated by hyphens.
/// - **`tracestate` dependency**: If `traceparent` fails to parse, this method returns `Err(())`; W3C Trace Context § 3.3
impl<'a> core::convert::TryFrom<Headers<'a>> for SpanContext {
    type Error = ();

    fn try_from(value: Headers<'a>) -> Result<Self, Self::Error> {
        let (version, rest) = value.traceparent.split_once('-').ok_or(())?;

        match version {
            "00" => {
                let (trace, rest) = rest.split_once('-').ok_or(())?;
                let (span, flags) = rest.split_once('-').ok_or(())?;

                Ok(SpanContext {
                    span_id: SpanId::from_hex(span)?,
                    trace_id: TraceId::from_hex(trace)?,
                    trace_flags: TraceFlags::from_hex(flags)?,
                    trace_state: value
                        .tracestate
                        .as_deref()
                        .map(Rc::from)
                        .unwrap_or_default(),
                })
            }
            _ => Err(()),
        }
    }
}

impl<'a> Headers<'a> {
    pub fn new(traceparent: &'a str, tracestate: Option<&'a str>) -> Self {
        Headers {
            traceparent: Str::Borrowed(traceparent),
            tracestate: tracestate.map(Str::Borrowed),
        }
    }

    /// Extracts W3C `traceparent` and `tracestate` headers from comma-separated key-value pairs
    /// emitted by [SQLCommenter](https://google.github.io/sqlcommenter).
    ///
    /// Per [W3C Trace Context](https://www.w3.org/TR/trace-context), `traceparent` is required;
    /// if it is missing, any accompanying `tracestate` is ignored and this returns `None`.
    fn from_comment(comment: &'a str) -> Option<Headers<'a>> {
        let mut traceparent = None;
        let mut tracestate = None;

        for (k, v) in CommenterIterator::new(comment) {
            if k == "traceparent" {
                traceparent = Some(v);
            } else if k == "tracestate" {
                tracestate = Some(v);
            }
        }

        Some(Headers {
            traceparent: traceparent?,
            tracestate,
        })
    }

    /// Extracts headers from a comment at the head (front/left) of a slice.
    #[allow(dead_code)]
    pub fn from_head(sql: &'a str) -> Option<Headers<'a>> {
        let comment = sql.trim_start().strip_prefix("/*")?;
        let (comment, _) = comment.split_once("*/")?;
        Self::from_comment(comment)
    }

    /// Extracts headers from a comment at the tail (back/right) of a slice.
    ///
    /// Compatible with [SQLCommenter](https://google.github.io/sqlcommenter).
    pub fn from_tail(sql: &'a str) -> Option<Headers<'a>> {
        let comment = sql.trim_end().strip_suffix("*/")?;
        let (_, comment) = comment.rsplit_once("/*")?;
        Self::from_comment(comment)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StackEntry {
    /// C1: Transaction caller context (from `BEGIN` comment/GUC or late-bound `SET LOCAL`).
    TransactionCaller(SpanContext),
    /// T: Generated context for the `TRANSACTION` span.
    TransactionSpan(SpanContext),
    /// C2 or C_leader: Statement caller context (from statement comment/GUC or parallel leader).
    StatementCaller(SpanContext),
    /// S1, S2..Sn, or S_worker: Generated context for a statement span.
    StatementSpan {
        context: SpanContext,
        depth: usize,
        scope_budget: u32,
    },
}

impl StackEntry {
    fn context(&self) -> &SpanContext {
        match self {
            Self::TransactionCaller(c)
            | Self::TransactionSpan(c)
            | Self::StatementCaller(c)
            | Self::StatementSpan { context: c, .. } => c,
        }
    }
}

thread_local! {
    /// Active contexts ordered from Root (`index 0`) to Current (`last`):
    /// `[C1?, T?, C2?, S1?, S2?, ...]` (or `[C_leader?, S_worker?]` in parallel workers).
    static CONTEXT_STACK: RefCell<Vec<StackEntry>> = const { RefCell::new(Vec::new()) };

    /// Tracks the starting byte offset of the unparsed remainder in a multi-statement simple query batch.
    ///
    /// Since PostgreSQL 18, `PlannedStmt::stmt_location` advances past leading comments to the first SQL keyword.
    /// The value here marks where the previous statement ended so leading comments can be recovered without backward scanning.
    /// It also anchors synthetic error spans when a batch aborts.
    static BATCH_CURSOR: Cell<usize> = const { Cell::new(0) };

    /// Whether the backend is currently inside a `COMMIT` or `ROLLBACK` utility statement.
    static IN_COMMIT_OR_ROLLBACK: Cell<bool> = const { Cell::new(false) };

    /// Whether the current transaction was started explicitly via `BEGIN` / `START`.
    static IS_EXPLICIT_TRANSACTION: Cell<bool> = const { Cell::new(false) };

    /// Statement execution depth (`0` when idle, `1` for top-level statement, `> 1` for nested).
    static STATEMENT_DEPTH: Cell<usize> = const { Cell::new(0) };

    /// Whether `[C1?, T?]` has already been resolved for the current transaction.
    static TRANSACTION_RESOLVED: Cell<bool> = const { Cell::new(false) };
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
    let reached_end = sql
        .get(next_pos..)
        .is_none_or(|s| s.trim_ascii_start().is_empty());

    BATCH_CURSOR.set(if reached_end { 0 } else { next_pos });
}

/// Returns the unparsed remainder of `debug_query_string`.
fn batch_remainder() -> Option<&'static str> {
    let query = unsafe { pg_sys::debug_query_string };
    if query.is_null() {
        return None;
    }
    let sql = unsafe { ffi::CStr::from_ptr(query) }.to_str().ok()?;
    Some(sql.get(BATCH_CURSOR.get()..).unwrap_or(sql))
}

/// Extracts incoming trace context for a statement, preferring SQL comments over session GUCs.
fn take_statement_context(
    pstmt: Option<&pg_sys::PlannedStmt>,
    sql: Option<&str>,
) -> Option<SpanContext> {
    // Consume and clear any pending session context GUCs.
    let guc_ctx = crate::config::take_trace_context();

    let comment_ctx = sql
        .filter(|_| crate::config::sqlcommenter())
        .and_then(|s| {
            let (start, len) = pstmt.map_or((None, None), |p| {
                (
                    usize::try_from(p.stmt_location).ok(),
                    usize::try_from(p.stmt_len).ok().filter(|&l| l > 0),
                )
            });
            let body = match (start, len) {
                (Some(loc), Some(l)) => s.get(loc..loc + l),
                (Some(loc), None) => s.get(loc..),
                (None, _) => None,
            }
            .unwrap_or(s);

            // Trailing comments (SQLCommenter) take precedence over leading comments.
            //
            // Since PostgreSQL 18, `PlannedStmt::stmt_location` points to the first SQL keyword.
            // Leading comments therefore reside at the tail of the area between the previous
            // statement's terminating delimiter (`BATCH_CURSOR`) and `start`.
            std::cfg_select! {
                any(feature = "pg15", feature = "pg16", feature = "pg17") => {
                    Headers::from_tail(body).or_else(|| Headers::from_head(body))
                }
                _ => {
                    Headers::from_tail(body).or_else(|| {
                        let prefix = s.get(BATCH_CURSOR.get()..start?)?;
                        Headers::from_tail(prefix)
                    })
                }
            }
        })
        .and_then(|h| SpanContext::try_from(h).ok());

    comment_ctx.or(guc_ctx)
}

/// Resolves `[C1?, T?]` at most once when inside a transaction block.
fn ensure_transaction_context() {
    if TRANSACTION_RESOLVED.get() || !unsafe { pg_sys::IsTransactionBlock() } {
        return;
    }

    CONTEXT_STACK.with_borrow_mut(|s| {
        // Add `C1` to the stack if `process_utility_start()` did not find it attached to `BEGIN`.
        if s.is_empty()
            && let Some(c1) = crate::config::take_trace_context()
        {
            s.push(StackEntry::TransactionCaller(c1));
        }

        // Add `T` to the stack when transaction spans are enabled and exported.
        if crate::config::export_traces() && crate::config::trace_transactions() {
            let t = match s.last() {
                Some(parent) => SpanContext::child_of(parent.context()),
                None => SpanContext::root(),
            };
            s.push(StackEntry::TransactionSpan(t));
        }
    });

    TRANSACTION_RESOLVED.set(true);
}

fn push_statement_context(pstmt: Option<&pg_sys::PlannedStmt>, sql: Option<&str>) {
    ensure_transaction_context();
    let stmt_caller = take_statement_context(pstmt, sql);

    CONTEXT_STACK.with_borrow_mut(|s| {
        if let Some(c2) = stmt_caller {
            s.push(StackEntry::StatementCaller(c2));
        }

        if crate::config::export_traces() && crate::config::trace_statements() {
            let s1 = match s.last() {
                Some(parent) => SpanContext::child_of(parent.context()),
                None => SpanContext::root(),
            };
            s.push(StackEntry::StatementSpan {
                context: s1,
                depth: 1,
                scope_budget: crate::config::nested_statements_request()
                    .min(crate::config::nested_statements_max()),
            });
        }
    });
}

fn push_nested_statement_context(depth: usize) {
    if !crate::config::export_traces() || !crate::config::trace_statements() {
        return;
    }

    CONTEXT_STACK.with_borrow_mut(|s| {
        if let [
            rest @ ..,
            StackEntry::StatementSpan {
                context: parent_ctx,
                depth: parent_depth,
                scope_budget: parent_budget,
            },
        ] = s.as_mut_slice()
            && *parent_depth == depth - 1
        {
            // Read the routine-local nesting limit, if any.
            let limit = crate::config::nested_statements_request()
                .min(crate::config::nested_statements_max());

            // Clamp the budget of the context of the already-executing routine.
            // Return any excess to the context prior to that one, if any.
            if *parent_budget > limit {
                let excess = *parent_budget - limit;
                *parent_budget = limit;

                if let Some(StackEntry::StatementSpan {
                    scope_budget: grandparent_budget,
                    ..
                }) = rest.last_mut()
                {
                    *grandparent_budget += excess;
                }
            }

            if *parent_budget > 0 {
                // Pay for this push onto the stack.
                *parent_budget -= 1;

                // Pass on ownership of the remaining budget.
                let child_budget = std::mem::take(parent_budget);
                let child_ctx = SpanContext::child_of(parent_ctx);

                s.push(StackEntry::StatementSpan {
                    context: child_ctx,
                    depth,
                    scope_budget: child_budget,
                });
            }
        }
    });
}

fn pop_statement_contexts() {
    CONTEXT_STACK.with_borrow_mut(|s| {
        while s.last().is_some_and(|e| {
            matches!(
                e,
                StackEntry::StatementCaller(_) | StackEntry::StatementSpan { .. }
            )
        }) {
            s.pop();
        }
    });
}

fn pop_nested_statement_context(depth: usize) {
    CONTEXT_STACK.with_borrow_mut(|s| {
        if s.last()
            .is_some_and(|e| matches!(e, StackEntry::StatementSpan { depth: d, .. } if *d == depth))
            && let Some(StackEntry::StatementSpan {
                scope_budget: unused,
                ..
            }) = s.pop()
            && let Some(StackEntry::StatementSpan {
                scope_budget: parent_budget,
                ..
            }) = s.last_mut()
        {
            *parent_budget += unused;
        }
    });
}

fn reset_transaction_state() {
    STATEMENT_DEPTH.set(0);
    IS_EXPLICIT_TRANSACTION.set(false);
    TRANSACTION_RESOLVED.set(false);
    IN_COMMIT_OR_ROLLBACK.set(false);
    CONTEXT_STACK.with_borrow_mut(Vec::clear);
}

/// Returns the current statement nesting depth, `0` when idle.
pub(crate) fn depth() -> usize {
    STATEMENT_DEPTH.get()
}

/// Returns `true` while executing a `COMMIT` or `ROLLBACK` utility statement.
pub(crate) fn in_commit_or_rollback() -> bool {
    IN_COMMIT_OR_ROLLBACK.get()
}

/// Returns `true` when an explicit transaction block is aborting an intermediate statement
/// (`TBLOCK_ABORT`) and remains alive awaiting `ROLLBACK` or `COMMIT`.
pub(crate) fn is_transaction_retained_on_abort() -> bool {
    IS_EXPLICIT_TRANSACTION.get()
        && unsafe { pg_sys::IsTransactionBlock() }
        && !IN_COMMIT_OR_ROLLBACK.get()
}

/// Returns the currently active [`SpanContext`], if any.
pub(crate) fn current() -> Option<SpanContext> {
    CONTEXT_STACK.with_borrow(|s| s.last().map(|e| e.context().clone()))
}

/// Returns the active statement span's `(context, parent_span_id, link_context)` at the current depth, if any.
///
/// `link_context` is `Some(T)` iff the stack ends with `[..., TransactionSpan(T), StatementCaller(C2), StatementSpan(S1)]`.
pub(crate) fn statement() -> Option<(SpanContext, Option<SpanId>, Option<SpanContext>)> {
    let target_depth = STATEMENT_DEPTH.get().max(1);
    CONTEXT_STACK.with_borrow(|s| match s.as_slice() {
        [rest @ .., StackEntry::StatementSpan { context, depth, .. }] if *depth == target_depth => {
            let parent_id = rest.last().map(|e| e.context().span_id);
            let link_ctx = match rest {
                [
                    ..,
                    StackEntry::TransactionSpan(tx),
                    StackEntry::StatementCaller(_),
                ] if *depth == 1 => Some(tx.clone()),
                _ => None,
            };
            Some((context.clone(), parent_id, link_ctx))
        }
        _ => None,
    })
}

/// Returns the active transaction span's `(T, parent_span_id)`, if any.
pub(crate) fn transaction() -> Option<(SpanContext, Option<SpanId>)> {
    CONTEXT_STACK.with_borrow(|s| {
        let pos = s
            .iter()
            .position(|e| matches!(e, StackEntry::TransactionSpan(_)))?;
        let ctx = s[pos].context().clone();
        let parent_id = pos
            .checked_sub(1)
            .and_then(|p| s.get(p))
            .map(|e| e.context().span_id);
        Some((ctx, parent_id))
    })
}

/// Called from [`pg_sys::emit_log_hook`] before `tracing` and `logging`.
///
/// When a statement fails during parsing or planning (before `ExecutorStart` or `ProcessUtility`
/// is reached), this resolves the context for error log correlation and synthetic error spans.
pub(crate) fn emit_log(record: &pg_sys::ErrorData) {
    use pg_sys::errcodes::PgSqlErrorCode;

    if record.elevel < pg_sys::PGERROR as i32
        || record.sqlerrcode == PgSqlErrorCode::ERRCODE_SUCCESSFUL_COMPLETION as i32
    {
        return;
    }

    if STATEMENT_DEPTH.get() == 0
        && !IN_COMMIT_OR_ROLLBACK.get()
        && !unsafe { pg_sys::IsAbortedTransactionBlockState() }
    {
        push_statement_context(None, batch_remainder());
    }
}

/// Called from [`pg_sys::ExecutorStart_hook`] before `tracing` when telemetry is enabled.
pub(crate) fn executor_start(pstmt: &pg_sys::PlannedStmt, sql: Option<&str>) {
    if IN_COMMIT_OR_ROLLBACK.get() {
        return;
    }

    let depth = STATEMENT_DEPTH.get() + 1;
    STATEMENT_DEPTH.set(depth);

    if unsafe { pg_sys::ParallelWorkerNumber >= 0 } {
        if depth == 1
            && let Some(c_leader) = crate::config::read_trace_context()
        {
            CONTEXT_STACK.with_borrow_mut(|s| {
                s.push(StackEntry::StatementCaller(c_leader.clone()));
                if crate::config::export_traces()
                    && crate::config::trace_statements()
                    && crate::config::trace_parallel_workers()
                {
                    let s_worker = SpanContext::child_of(&c_leader);
                    s.push(StackEntry::StatementSpan {
                        context: s_worker,
                        depth: 1,
                        scope_budget: 0,
                    });
                }
            });
        }
        return;
    }

    if depth == 1 {
        push_statement_context(Some(pstmt), sql);
    } else {
        push_nested_statement_context(depth);
    }

    if pstmt.parallelModeNeeded
        && let Some(ctx) = current()
    {
        crate::config::set_trace_context(&ctx.w3c_traceparent(), &ctx.trace_state);
    }
}

/// Called from [`pg_sys::ExecutorEnd_hook`] after `tracing` when telemetry is enabled.
pub(crate) fn executor_end(pstmt: &pg_sys::PlannedStmt, sql: Option<&str>) {
    let depth = STATEMENT_DEPTH.get();
    if IN_COMMIT_OR_ROLLBACK.get() || depth == 0 {
        return;
    }
    STATEMENT_DEPTH.set(depth - 1);

    if unsafe { pg_sys::ParallelWorkerNumber >= 0 } {
        if depth == 1 {
            pop_statement_contexts();
        }
        return;
    }

    if depth == 1 {
        pop_statement_contexts();
    } else {
        pop_nested_statement_context(depth);
    }

    if pstmt.parallelModeNeeded {
        crate::config::clear_trace_context();
    }

    if depth == 1
        && let Some(src) = sql
    {
        batch_advance_cursor(src, pstmt.stmt_location, pstmt.stmt_len);
    }
}

/// Called from [`pg_sys::ProcessUtility_hook`] before `tracing` and downstream hooks.
pub(crate) fn process_utility_start(
    pstmt: &pg_sys::PlannedStmt,
    tree: &pg_sys::Node,
    sql: Option<&str>,
) {
    use pg_sys::{NodeTag, TransactionStmtKind as StmtKind};

    let depth = STATEMENT_DEPTH.get();

    match tree
        .try_as::<pg_sys::TransactionStmt>()
        .map(|xact| xact.kind)
    {
        Some(StmtKind::TRANS_STMT_BEGIN | StmtKind::TRANS_STMT_START) => {
            IS_EXPLICIT_TRANSACTION.set(true);
            if depth == 0
                && !TRANSACTION_RESOLVED.get()
                && let Some(c1) = take_statement_context(Some(pstmt), sql)
            {
                // Add `C1` to the stack if it is attached to this statement.
                // Add `T` to the stack when transaction spans are enabled and exported.
                CONTEXT_STACK.with_borrow_mut(|s| {
                    if crate::config::export_traces() && crate::config::trace_transactions() {
                        let t = SpanContext::child_of(&c1);
                        s.push(StackEntry::TransactionCaller(c1));
                        s.push(StackEntry::TransactionSpan(t));
                    } else {
                        s.push(StackEntry::TransactionCaller(c1));
                    }
                });
                TRANSACTION_RESOLVED.set(true);
            }
        }
        Some(StmtKind::TRANS_STMT_COMMIT | StmtKind::TRANS_STMT_ROLLBACK) => {
            let new_depth = depth + 1;
            STATEMENT_DEPTH.set(new_depth);
            IN_COMMIT_OR_ROLLBACK.set(true);

            if new_depth == 1 {
                push_statement_context(Some(pstmt), sql);
            }
        }
        _ if tree.is_a(NodeTag::T_VariableSetStmt) => {}
        _ => {
            let new_depth = depth + 1;
            STATEMENT_DEPTH.set(new_depth);
            if new_depth == 1 {
                push_statement_context(Some(pstmt), sql);
            } else {
                push_nested_statement_context(new_depth);
            }
        }
    }
}

/// Called from [`pg_sys::ProcessUtility_hook`] after downstream hooks and `tracing`.
pub(crate) fn process_utility_end(
    pstmt: &pg_sys::PlannedStmt,
    tree: &pg_sys::Node,
    sql: Option<&str>,
) {
    use pg_sys::{NodeTag, TransactionStmtKind as StmtKind};

    let depth = STATEMENT_DEPTH.get();

    match tree
        .try_as::<pg_sys::TransactionStmt>()
        .map(|xact| xact.kind)
    {
        Some(StmtKind::TRANS_STMT_BEGIN | StmtKind::TRANS_STMT_START) => {
            if depth == 0
                && let Some(src) = sql
            {
                batch_advance_cursor(src, pstmt.stmt_location, pstmt.stmt_len);
            }
        }
        Some(StmtKind::TRANS_STMT_COMMIT | StmtKind::TRANS_STMT_ROLLBACK) => {
            if depth == 1
                && let Some(src) = sql
            {
                batch_advance_cursor(src, pstmt.stmt_location, pstmt.stmt_len);
            }
            if !unsafe { pg_sys::IsTransactionState() } {
                reset_transaction_state();
            }
        }
        _ if tree.is_a(NodeTag::T_VariableSetStmt) => {
            if depth == 0
                && let Some(src) = sql
            {
                batch_advance_cursor(src, pstmt.stmt_location, pstmt.stmt_len);
            }
        }
        _ => {
            if depth == 0 {
                return;
            }
            STATEMENT_DEPTH.set(depth - 1);
            if depth == 1 {
                pop_statement_contexts();
                if let Some(src) = sql {
                    batch_advance_cursor(src, pstmt.stmt_location, pstmt.stmt_len);
                }
            } else {
                pop_nested_statement_context(depth);
            }
        }
    }
}

/// Called unconditionally via [`pg_sys::RegisterSubXactCallback`] on subtransaction abort.
pub(crate) fn subtransaction_abort() {
    STATEMENT_DEPTH.set(0);
    pop_statement_contexts();
}

/// Called unconditionally via [`pg_sys::RegisterXactCallback`] on transaction commit.
pub(crate) fn transaction_commit() {
    reset_transaction_state();
}

/// Called unconditionally via [`pg_sys::RegisterXactCallback`] on transaction abort.
pub(crate) fn transaction_abort() {
    BATCH_CURSOR.set(0);

    if is_transaction_retained_on_abort() {
        STATEMENT_DEPTH.set(0);
        pop_statement_contexts();
    } else {
        reset_transaction_state();
    }
}

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    #[cfg(test)]
    use super::{Headers, TraceFlags};
    use super::{SpanContext, SpanId, TraceId};
    use googletest::prelude::*;

    #[test]
    fn test_headers_from_head() {
        // Comment at head of statement body (Postgres 13–17)
        assert_that!(
            Headers::from_head(
                "/* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',tracestate='congo=t61rcWkgMzE' */ SELECT * FROM users"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                Some("congo=t61rcWkgMzE"),
            ))),
        );

        // Comment at head without tracestate
        assert_that!(
            Headers::from_head(
                "/* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ SELECT * FROM users"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None,
            ))),
        );

        // Comment at head with leading whitespace
        assert_that!(
            Headers::from_head(
                "  \t\n /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ SELECT 1"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None,
            ))),
        );

        // Multiline comment at head
        assert_that!(
            Headers::from_head(
                "/*\n traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',\ntracestate='congo=t61rcWkgMzE'\n*/ SELECT 1"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                Some("congo=t61rcWkgMzE"),
            ))),
        );

        // Ignore comment at tail (not at head)
        assert_that!(
            Headers::from_head(
                "SELECT 1 /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */"
            ),
            none(),
        );

        // Ignore comment embedded in the middle
        assert_that!(
            Headers::from_head(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ WHERE id = 1"
            ),
            none(),
        );

        // Empty comment or no-traceparent comment returns None
        assert_that!(Headers::from_head("/**/ SELECT 1"), none());
        assert_that!(
            Headers::from_head("/* route='/users',action='index' */ SELECT 1"),
            none(),
        );
        assert_that!(
            Headers::from_head("/* tracestate='congo=t61rcWkgMzE' */ SELECT 1"),
            none(),
        );

        // Multiple comments: comment at head takes precedence when inspecting head
        assert_that!(
            Headers::from_head(
                "/* traceparent='00-11111111111111111111111111111111-1111111111111111-01' */ SELECT 1 /* traceparent='00-22222222222222222222222222222222-2222222222222222-01' */"
            ),
            some(eq(&Headers::new(
                "00-11111111111111111111111111111111-1111111111111111-01",
                None,
            ))),
        );
    }

    #[test]
    fn test_headers_from_tail() {
        // Comment at tail of statement body (all Postgres versions)
        assert_that!(
            Headers::from_tail(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',tracestate='congo=t61rcWkgMzE' */"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                Some("congo=t61rcWkgMzE"),
            ))),
        );

        // Comment at tail without tracestate
        assert_that!(
            Headers::from_tail(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None,
            ))),
        );

        // Comment at tail followed by whitespace
        assert_that!(
            Headers::from_tail(
                "SELECT 1 /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */  \t\n "
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None,
            ))),
        );

        // Multiline comment at tail
        assert_that!(
            Headers::from_tail(
                "SELECT 1 /*\n traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',\ntracestate='congo=t61rcWkgMzE'\n*/"
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                Some("congo=t61rcWkgMzE"),
            ))),
        );

        // Postgres 18+ prefix slice: comment at tail of prefix (immediately preceding statement keyword)
        assert_that!(
            Headers::from_tail(
                "/* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ "
            ),
            some(eq(&Headers::new(
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                None,
            ))),
        );

        // Postgres 18+ prefix slice: empty statement barrier (ends with ';', not '*/') ignored
        assert_that!(Headers::from_tail(" /* isolated */ ; "), none(),);

        // Statement slice with terminating semicolon ignored (slices stop before ';')
        assert_that!(
            Headers::from_tail(
                "SELECT 1 /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */;"
            ),
            none(),
        );

        // Ignore comment at head (not at tail)
        assert_that!(
            Headers::from_tail(
                "/* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ SELECT 1"
            ),
            none(),
        );

        // Ignore comment embedded in the middle of slice
        assert_that!(
            Headers::from_tail(
                "SELECT * FROM users /* traceparent='00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01' */ WHERE id = 1"
            ),
            none(),
        );

        // Empty comment or no-traceparent comment returns None
        assert_that!(Headers::from_tail("SELECT 1 /**/"), none());
        assert_that!(
            Headers::from_tail("SELECT 1 /* route='/users',action='index' */"),
            none(),
        );
        assert_that!(
            Headers::from_tail("SELECT * FROM users /* tracestate='congo=t61rcWkgMzE' */"),
            none(),
        );

        // Multiple comments: comment at tail takes precedence
        assert_that!(
            Headers::from_tail(
                "/* traceparent='00-11111111111111111111111111111111-1111111111111111-01' */ SELECT 1 /* traceparent='00-22222222222222222222222222222222-2222222222222222-01' */"
            ),
            some(eq(&Headers::new(
                "00-22222222222222222222222222222222-2222222222222222-01",
                None,
            ))),
        );
    }

    #[test]
    fn test_context_try_from_headers_valid() {
        struct Case<'a> {
            name: &'a str,
            traceparent: &'a str,
            tracestate: Option<&'a str>,
            expected_trace: TraceId,
            expected_span: SpanId,
            expected_flags: TraceFlags,
        }

        let cases = [
            Case {
                name: "sampled",
                traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                tracestate: None,
                expected_trace: TraceId(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736),
                expected_span: SpanId(0x00f0_67aa_0ba9_02b7),
                expected_flags: TraceFlags::W3C_SAMPLED,
            },
            Case {
                name: "unsampled",
                traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00",
                tracestate: None,
                expected_trace: TraceId(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736),
                expected_span: SpanId(0x00f0_67aa_0ba9_02b7),
                expected_flags: TraceFlags::default(),
            },
            Case {
                name: "unsampled with state",
                traceparent: "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00",
                tracestate: Some("congo=t61rcWkgMzE"),
                expected_trace: TraceId(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736),
                expected_span: SpanId(0x00f0_67aa_0ba9_02b7),
                expected_flags: TraceFlags::default(),
            },
        ];

        for case in cases {
            let headers = Headers::new(case.traceparent, case.tracestate);

            scoped_trace!("{}", case.name);
            assert_that!(
                SpanContext::try_from(headers),
                ok(eq(&SpanContext {
                    span_id: case.expected_span,
                    trace_id: case.expected_trace,
                    trace_flags: case.expected_flags,
                    trace_state: case.tracestate.map(std::rc::Rc::from).unwrap_or_default(),
                })),
            );
        }
    }

    #[test]
    fn test_context_try_from_headers_invalid() {
        for (name, traceparent) in [
            (
                "bad version",
                "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            ),
            (
                "bad trace id",
                "00-4bf92f3577b34da6a3ce929d0e0e473g-00f067aa0ba902b7-01",
            ),
            (
                "bad span id",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902bg-01",
            ),
            (
                "bad flags",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-0g",
            ),
            ("too short", "00-4bf92f3577b34da6a3ce-00f067aa-01"),
            ("missing fields", "00-4bf92f3577b34da6a3ce929d0e0e4736"),
        ] {
            scoped_trace!("{name}");
            assert_that!(
                SpanContext::try_from(Headers::new(traceparent, None)),
                err(anything()),
            );
        }
    }

    #[test]
    fn test_context_w3c_traceparent_roundtrip() {
        let trace_id = TraceId(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736);
        let span_id = SpanId(0x00f0_67aa_0ba9_02b7);

        let sampled = SpanContext {
            trace_id,
            span_id,
            trace_flags: TraceFlags::W3C_SAMPLED,
            trace_state: std::rc::Rc::default(),
        };
        assert_that!(
            sampled.w3c_traceparent(),
            eq("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
        assert_that!(
            SpanContext::try_from(Headers::new(&sampled.w3c_traceparent(), None)),
            ok(eq(&sampled))
        );

        let unsampled = SpanContext {
            trace_id,
            span_id,
            trace_flags: TraceFlags::default(),
            trace_state: std::rc::Rc::default(),
        };
        assert_that!(
            unsampled.w3c_traceparent(),
            eq("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00")
        );
        assert_that!(
            SpanContext::try_from(Headers::new(&unsampled.w3c_traceparent(), None)),
            ok(eq(&unsampled))
        );
    }

    #[pgrx::pg_test]
    fn test_context_root_and_child() {
        let root = SpanContext::root();
        assert!(root.span_id.is_valid());
        assert!(root.trace_id.is_valid());
        assert!(root.trace_flags.is_random());
        assert!(root.trace_flags.is_sampled());
        assert!(root.trace_state.is_empty());

        let child = SpanContext::child_of(&root);
        assert_that!(child.trace_id, eq(root.trace_id));
        assert_that!(child.trace_flags, eq(root.trace_flags));
        assert_that!(&child.trace_state, eq(&root.trace_state));
        assert_that!(child.span_id, not(eq(root.span_id)));
        assert!(child.span_id.is_valid());
    }

    #[pgrx::pg_test]
    fn test_span_id_construction() {
        let span1 = SpanId::random();
        assert_that!(span1, eq(span1));
        assert!(span1.is_valid());

        let span2 = SpanId::random();
        assert_that!(span2, eq(span2));
        assert!(span2.is_valid());

        assert_that!(span1, not(eq(span2)), "generated");
        assert_that!(span2, not(eq(span1)), "generated");
    }

    #[test]
    fn test_span_id_conversions() {
        let span = SpanId(0x0123_4567_89ab_cdef);
        let v: Vec<u8> = span.into();
        assert_that!(v, eq(&vec![0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]));
    }

    #[test]
    fn test_span_id_formatting() {
        let span = SpanId(0x00f0_67aa_0ba9_02b7);
        assert_that!(format!("{:016x}", span), eq("00f067aa0ba902b7"));
        assert_that!(format!("{:x}", span), eq("00f067aa0ba902b7"));
        assert_that!(format!("{:?}", span), eq("SpanId(00f067aa0ba902b7)"));
    }

    #[pgrx::pg_test]
    fn test_trace_id_construction() {
        let trace1 = TraceId::random();
        assert_that!(trace1, eq(trace1));
        assert!(trace1.is_valid());

        let trace2 = TraceId::random();
        assert_that!(trace2, eq(trace2));
        assert!(trace2.is_valid());

        assert_that!(trace1, not(eq(trace2)), "generated");
        assert_that!(trace2, not(eq(trace1)), "generated");
    }

    #[test]
    fn test_trace_id_conversions() {
        let trace = TraceId(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let v: Vec<u8> = trace.into();
        assert_that!(
            v,
            eq(&vec![
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef,
            ])
        );
    }

    #[test]
    fn test_trace_id_formatting() {
        let trace = TraceId(0x4bf9_2f35_77b3_4da6_a3ce_929d_0e0e_4736);
        assert_that!(
            format!("{:032x}", trace),
            eq("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert_that!(
            format!("{:x}", trace),
            eq("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert_that!(
            format!("{:?}", trace),
            eq("TraceId(4bf92f3577b34da6a3ce929d0e0e4736)")
        );
    }

    #[test]
    fn test_trace_flags_conversions() {
        let flags = TraceFlags::W3C_SAMPLED;
        let flags_u32: u32 = flags.into();
        assert_that!(flags_u32, eq(1u32));
    }

    #[test]
    fn test_trace_flags_formatting() {
        let flags = TraceFlags::W3C_SAMPLED;
        assert_that!(format!("{:02x}", flags), eq("01"));
        assert_that!(format!("{:x}", flags), eq("01"));
        assert_that!(format!("{:?}", flags), eq("TraceFlags(01)"));
    }

    #[test]
    fn test_trace_flags_bitor() {
        let flags = TraceFlags::W3C_RANDOM | TraceFlags::W3C_SAMPLED;
        assert_that!(flags.is_random(), eq(true));
        assert_that!(flags.is_sampled(), eq(true));
        assert_that!(format!("{:x}", flags), eq("03"));
    }
}
