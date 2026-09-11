// SPDX-License-Identifier: MIT

mod context;

use self::context::{SpanContext, SpanId, TraceFlags, TraceId};
use crate::otlp::*;
use pgrx::pg_sys;
use std::{borrow, cell, ffi, time};

thread_local! {
    static EXECUTION_DEPTH: i32 = 0;
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

    // Register to be called after every transaction ends; commit and rollback are already done.
    unsafe { pg_sys::RegisterXactCallback(Some(xact_callback), std::ptr::null_mut()) };
    unsafe { pg_sys::RegisterSubXactCallback(Some(subxact_callback), std::ptr::null_mut()) };

    #[pgrx::pg_guard]
    unsafe extern "C-unwind" fn xact_callback(
        event: pg_sys::XactEvent::Type,
        _arg: pgrx::void_mut_ptr,
    ) {
        use pg_sys::XactEvent::*;
        match event {
            XACT_EVENT_ABORT => transaction_abort(),
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

    /// This `ExecutorStart` hook is called before fetching rows of a statement;
    /// planning is already done.
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
}

fn executor_start(query: Option<&pg_sys::QueryDesc>) {
    let now = time::SystemTime::now();
    let Some(qd) = query else { return };

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
}

fn process_utility<F>(pstmt: Option<&pg_sys::PlannedStmt>, sql: Option<&ffi::CStr>, hooks: F)
where
    F: FnOnce(),
{
    let now = time::SystemTime::now();
    let sql = sql.and_then(|s| s.to_str().ok());

    hooks();
}

fn transaction_abort() {
    let now = time::SystemTime::now();
    let in_xact_block = unsafe { pg_sys::IsTransactionBlock() };
}

fn subxact_abort() {
    let now = time::SystemTime::now();
}

fn transaction_commit() {
    let now = time::SystemTime::now();
}

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use super::*;
    use googletest::prelude::*;
    use prost::Message;
    use std::{thread, time};

    #[pgrx::pg_test]
    fn simple_utility_single_span() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        // setup: clear the mock collector
        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute("CREATE TEMP TABLE t1 (id int)")?;

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        // retrieve trace records from the collector with timeout
        while !done && start.elapsed() < time::Duration::from_secs(1) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
            {
                done = body.is_empty() && !data.is_empty();
                if let Ok(decoded) = TracesData::decode(body) {
                    data.extend(decoded.resource_spans);
                }
            }
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("CREATE"))?;
        verify_that!(
            all_spans[0].attributes,
            contains(eq(&KeyValue::new_string("db.operation.name", "CREATE")))
        )
    }

    #[pgrx::pg_test]
    fn utility_with_context() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        // setup: clear the mock collector
        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute(
            "CREATE TEMP TABLE t2 (id int) \
                /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */",
        )?;

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < time::Duration::from_secs(1) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
            {
                done = body.is_empty() && !data.is_empty();
                if let Ok(decoded) = TracesData::decode(body) {
                    data.extend(decoded.resource_spans);
                }
            }
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("CREATE"))?;
        verify_that!(all_spans[0].trace_id, eq(&vec![0x11u8; 16]))?;
        verify_that!(all_spans[0].parent_span_id, eq(&vec![0x22u8; 8]))?;
        verify_that!(
            all_spans[0].attributes,
            contains(eq(&KeyValue::new_string("db.operation.name", "CREATE")))
        )
    }

    #[pgrx::pg_test]
    fn simple_query_single_span() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute("SELECT 1")?;

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < time::Duration::from_secs(1) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
            {
                done = body.is_empty() && !data.is_empty();
                if let Ok(decoded) = TracesData::decode(body) {
                    data.extend(decoded.resource_spans);
                }
            }
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("SELECT"))?;
        verify_that!(
            all_spans[0].attributes,
            contains(eq(&KeyValue::new_string("db.operation.name", "SELECT")))
        )
    }

    #[pgrx::pg_test]
    fn simple_query_in_transaction() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute("BEGIN")?;
        client.batch_execute("SELECT 1")?;
        client.batch_execute("COMMIT")?;

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < time::Duration::from_secs(2) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
                && let Ok(decoded) = TracesData::decode(body)
            {
                data.extend(decoded.resource_spans);
            }

            done = data
                .iter()
                .cloned()
                .flat_map(|rs| rs.scope_spans)
                .flat_map(|ss| ss.spans)
                .any(|s| s.name == "TRANSACTION");
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        let xact_span = all_spans.last().or_fail()?;
        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    parent_span_id: is_empty(),
                    ..
                }),
            ]
        )
    }

    #[pgrx::pg_test]
    fn batch_query_in_transaction() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute("BEGIN; SELECT 1; COMMIT")?; // single Simple Query message

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < time::Duration::from_secs(2) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
                && let Ok(decoded) = TracesData::decode(body)
            {
                data.extend(decoded.resource_spans);
            }

            done = data
                .iter()
                .cloned()
                .flat_map(|rs| rs.scope_spans)
                .flat_map(|ss| ss.spans)
                .any(|s| s.name == "TRANSACTION");
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        let xact_span = all_spans.last().or_fail()?;
        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    parent_span_id: is_empty(),
                    ..
                }),
            ]
        )
    }

    #[pgrx::pg_test]
    fn multiple_queries_in_transaction() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute("BEGIN")?;
        client.batch_execute("SELECT 1")?;
        client.batch_execute("SELECT 2")?;
        client.batch_execute("COMMIT")?;

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < time::Duration::from_secs(2) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
                && let Ok(decoded) = TracesData::decode(body)
            {
                data.extend(decoded.resource_spans);
            }

            done = data
                .iter()
                .cloned()
                .flat_map(|rs| rs.scope_spans)
                .flat_map(|ss| ss.spans)
                .any(|s| s.name == "TRANSACTION");
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        let xact_span = all_spans.last().or_fail()?;
        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    parent_span_id: is_empty(),
                    ..
                }),
            ]
        )
    }

    #[pgrx::pg_test]
    fn batch_multiple_queries_in_transaction() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute("BEGIN; SELECT 1; SELECT 2; COMMIT;")?;

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < time::Duration::from_secs(2) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
                && let Ok(decoded) = TracesData::decode(body)
            {
                data.extend(decoded.resource_spans);
            }

            done = data
                .iter()
                .cloned()
                .flat_map(|rs| rs.scope_spans)
                .flat_map(|ss| ss.spans)
                .any(|s| s.name == "TRANSACTION");
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        let xact_span = all_spans.last().or_fail()?;
        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    parent_span_id: is_empty(),
                    ..
                }),
            ]
        )
    }

    #[pgrx::pg_test]
    fn single_query_in_transaction_with_context() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute(
            "BEGIN /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */",
        )?;
        client.batch_execute("SELECT 1")?;
        client.batch_execute("COMMIT")?;

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < time::Duration::from_secs(2) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
                && let Ok(decoded) = TracesData::decode(body)
            {
                data.extend(decoded.resource_spans);
            }

            done = data
                .iter()
                .cloned()
                .flat_map(|rs| rs.scope_spans)
                .flat_map(|ss| ss.spans)
                .any(|s| s.name == "TRANSACTION");
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        let expected_trace_id = vec![0x11; 16];
        let expected_parent_span_id = vec![0x22; 8];
        let xact_span = all_spans.last().or_fail()?;

        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&expected_trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    trace_id: eq(&expected_trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    trace_id: eq(&expected_trace_id),
                    parent_span_id: eq(&expected_parent_span_id),
                    ..
                }),
            ]
        )
    }

    #[pgrx::pg_test]
    fn single_query_with_context_in_transaction() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute("BEGIN")?;
        client.batch_execute(
            "SELECT 1 /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */",
        )?;
        client.batch_execute("COMMIT")?;

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < time::Duration::from_secs(2) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
                && let Ok(decoded) = TracesData::decode(body)
            {
                data.extend(decoded.resource_spans);
            }

            done = data
                .iter()
                .cloned()
                .flat_map(|rs| rs.scope_spans)
                .flat_map(|ss| ss.spans)
                .any(|s| s.name == "TRANSACTION");
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        let override_trace_id = vec![0x11; 16];
        let override_parent_span_id = vec![0x22; 8];
        let xact_span = all_spans.last().or_fail()?;

        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&override_trace_id),
                    parent_span_id: eq(&override_parent_span_id),
                    links: elements_are![matches_pattern!(Link {
                        trace_id: eq(&xact_span.trace_id),
                        span_id: eq(&xact_span.span_id),
                        ..
                    })],
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    trace_id: not(eq(&override_trace_id)),
                    parent_span_id: is_empty(),
                    ..
                }),
            ]
        )
    }

    #[pgrx::pg_test]
    fn parent_override_in_transaction_with_context() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute(
            "BEGIN /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */",
        )?;
        client.batch_execute(
            "SELECT 1 /* traceparent='00-33333333333333333333333333333333-4444444444444444-01' */",
        )?;
        client.batch_execute("SELECT 2")?;
        client.batch_execute("COMMIT")?;

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < time::Duration::from_secs(2) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
                && let Ok(decoded) = TracesData::decode(body)
            {
                data.extend(decoded.resource_spans);
            }

            done = data
                .iter()
                .cloned()
                .flat_map(|rs| rs.scope_spans)
                .flat_map(|ss| ss.spans)
                .any(|s| s.name == "TRANSACTION");
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        let xact_span = all_spans.last().or_fail()?;
        let xact_trace_id = vec![0x11; 16];
        let xact_parent_span_id = vec![0x22; 8];
        let override_trace_id = vec![0x33; 16];
        let override_parent_span_id = vec![0x44; 8];

        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&override_trace_id),
                    parent_span_id: eq(&override_parent_span_id),
                    links: elements_are![matches_pattern!(Link {
                        trace_id: eq(&xact_trace_id),
                        span_id: eq(&xact_span.span_id),
                        ..
                    })],
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&xact_trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    trace_id: eq(&xact_trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    trace_id: eq(&xact_trace_id),
                    parent_span_id: eq(&xact_parent_span_id),
                    ..
                }),
            ]
        )
    }

    #[pgrx::pg_test]
    fn simple_query_error() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        for sql in &[
            // This fails while planning, when Postgres folds constants.
            "SELECT 1/0",
            // This fails while running, when Postgres produces rows.
            "WITH t(x) AS MATERIALIZED (SELECT 0) SELECT 1/x FROM t",
        ] {
            scoped_trace!("{sql}");

            let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
            let _ = reqwest::blocking::get(&endpoint);

            client.batch_execute("SET pg_otel.export = 'traces'")?;
            verify_that!(client.batch_execute(sql), err(anything()))?;

            let start = time::Instant::now();
            let mut data = Vec::new();
            let mut done = false;

            while !done && start.elapsed() < time::Duration::from_secs(2) {
                thread::sleep(time::Duration::from_millis(50));

                if let Ok(response) = reqwest::blocking::get(&endpoint)
                    && let Ok(body) = response.bytes()
                    && let Ok(decoded) = TracesData::decode(body)
                {
                    data.extend(decoded.resource_spans);
                }

                done = !data.is_empty();
            }

            let all_spans: Vec<Span> = data
                .into_iter()
                .flat_map(|rs| rs.scope_spans)
                .flat_map(|ss| ss.spans)
                .collect();

            verify_that!(
                all_spans,
                elements_are![matches_pattern!(Span {
                    name: eq("SELECT"),
                    status: eq(&Some(SpanStatus {
                        code: SpanStatusCode::Error as i32,
                        message: String::new(),
                    })),
                    attributes: container_eq([
                        KeyValue::new_string("db.operation.name", "SELECT"),
                        KeyValue::new_string("db.system.name", "postgresql"),
                        KeyValue::new_string("db.response.status_code", "22012"),
                    ]),
                    parent_span_id: is_empty(),
                    ..
                })]
            )?;
        }
        Ok(())
    }

    #[pgrx::pg_test]
    fn query_error_in_transaction() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        for sql in &[
            // This fails while planning, when Postgres folds constants.
            "SELECT 1/0",
            // This fails while running, when Postgres produces rows.
            "WITH t(x) AS MATERIALIZED (SELECT 0) SELECT 1/x FROM t",
        ] {
            scoped_trace!("{sql}");

            let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
            let _ = reqwest::blocking::get(&endpoint);

            client.batch_execute("SET pg_otel.export = 'traces'")?;
            client.batch_execute("BEGIN")?;
            verify_that!(client.batch_execute(sql), err(anything()))?;
            client.batch_execute("ROLLBACK")?;

            let start = time::Instant::now();
            let mut data = Vec::new();
            let mut done = false;

            while !done && start.elapsed() < time::Duration::from_secs(2) {
                thread::sleep(time::Duration::from_millis(50));

                if let Ok(response) = reqwest::blocking::get(&endpoint)
                    && let Ok(body) = response.bytes()
                    && let Ok(decoded) = TracesData::decode(body)
                {
                    data.extend(decoded.resource_spans);
                }

                done = data
                    .iter()
                    .cloned()
                    .flat_map(|rs| rs.scope_spans)
                    .flat_map(|ss| ss.spans)
                    .any(|s| s.name == "TRANSACTION");
            }

            let all_spans: Vec<Span> = data
                .into_iter()
                .flat_map(|rs| rs.scope_spans)
                .flat_map(|ss| ss.spans)
                .collect();

            let xact_span = all_spans.last().or_fail()?;
            let error_status = Some(SpanStatus {
                code: SpanStatusCode::Error as i32,
                message: String::new(),
            });
            let ok_status = Some(SpanStatus {
                code: SpanStatusCode::Ok as i32,
                message: String::new(),
            });

            verify_that!(
                all_spans,
                elements_are![
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        status: eq(&error_status),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.response.status_code",
                            "22012",
                        ))),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("ROLLBACK"),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        status: eq(&ok_status),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("TRANSACTION"),
                        trace_id: not(is_empty()),
                        parent_span_id: is_empty(),
                        status: eq(&error_status),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.response.status_code",
                            "22012",
                        ))),
                        ..
                    }),
                ]
            )?;
        }
        Ok(())
    }

    #[pgrx::pg_test]
    fn transaction_commit_failure() -> Result<()> {
        let _lock = crate::acquire_test_lock();
        let mut client = crate::connect_test_client();

        let endpoint = format!("{}/test/traces", crate::exporter::endpoint());
        let _ = reqwest::blocking::get(&endpoint);

        client.batch_execute("CREATE TEMP TABLE p (id int PRIMARY KEY)")?;
        client.batch_execute(
            "CREATE TEMP TABLE c (p_id int REFERENCES p(id) DEFERRABLE INITIALLY DEFERRED)",
        )?;

        client.batch_execute("SET pg_otel.export = 'traces'")?;
        client.batch_execute("BEGIN")?;
        client.batch_execute("INSERT INTO c VALUES (999)")?;
        verify_that!(client.batch_execute("COMMIT"), err(anything()))?;

        let start = time::Instant::now();
        let mut data = Vec::new();
        let mut done = false;

        while !done && start.elapsed() < time::Duration::from_secs(2) {
            thread::sleep(time::Duration::from_millis(50));

            if let Ok(response) = reqwest::blocking::get(&endpoint)
                && let Ok(body) = response.bytes()
                && let Ok(decoded) = TracesData::decode(body)
            {
                data.extend(decoded.resource_spans);
            }

            done = data
                .iter()
                .cloned()
                .flat_map(|rs| rs.scope_spans)
                .flat_map(|ss| ss.spans)
                .any(|s| s.name == "TRANSACTION");
        }

        let all_spans: Vec<Span> = data
            .into_iter()
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect();

        let xact_span = all_spans.last().or_fail()?;
        let error_status = Some(SpanStatus {
            code: SpanStatusCode::Error as i32,
            message: String::new(),
        });
        let ok_status = Some(SpanStatus {
            code: SpanStatusCode::Ok as i32,
            message: String::new(),
        });

        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("INSERT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    status: eq(&ok_status),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    status: eq(&error_status),
                    attributes: contains(eq(&KeyValue::new_string(
                        "db.response.status_code",
                        "23503",
                    ))),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    parent_span_id: is_empty(),
                    status: eq(&error_status),
                    attributes: contains(eq(&KeyValue::new_string(
                        "db.response.status_code",
                        "23503",
                    ))),
                    ..
                }),
            ]
        )
    }
}
