// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn simple_query_single_span() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.simple_query("SELECT 1")?;

        let all_spans = harness.collect_spans(time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("SELECT"))?;
        verify_that!(all_spans[0].kind, eq(SpanKind::Server as i32))?;
        verify_that!(all_spans[0].status, is_empty())?;
        verify_that!(all_spans[0].parent_span_id, is_empty())?;
        verify_that!(
            all_spans[0].attributes,
            contains(eq(&KeyValue::new_string("db.operation.name", "SELECT")))
        )
    }

    #[pgrx::pg_test]
    fn query_resource_attached() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.simple_query("SELECT 1")?;

        let resource_spans = harness.collect_resource_spans(time::Duration::from_secs(1));
        verify_that!(resource_spans, len(gt(0)))?;
        let resource = resource_spans[0]
            .resource
            .as_ref()
            .expect("missing resource");
        verify_that!(
            resource.attributes,
            contains(eq(&KeyValue::new_string("service.name", "postgresql")))
        )?;
        verify_that!(
            resource.attributes,
            contains(eq(&KeyValue::new_string(
                "service.version",
                crate::config::service_version()
            )))
        )
    }

    #[pgrx::pg_test]
    fn query_with_comment_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.simple_query(
            "SELECT 1 /* traceparent='00-11111111111111111111111111111111-2222222222222222-01',tracestate='rojo=1' */",
        )?;

        let all_spans = harness.collect_spans(time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("SELECT"))?;
        verify_that!(all_spans[0].kind, eq(SpanKind::Server as i32))?;
        verify_that!(all_spans[0].status, is_empty())?;
        verify_that!(all_spans[0].trace_id, eq(&vec![0x11u8; 16]))?;
        verify_that!(all_spans[0].parent_span_id, eq(&vec![0x22u8; 8]))?;
        verify_that!(all_spans[0].trace_state, eq("rojo=1"))?;
        verify_that!(
            all_spans[0].attributes,
            contains(eq(&KeyValue::new_string("db.operation.name", "SELECT")))
        )
    }

    #[pgrx::pg_test]
    fn query_with_guc_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        for sqls in [
            // Four Query ('Q') messages
            &[
                "SET otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'",
                "SET otel.tracestate = 'rojo=1'",
                "SELECT 1",
                "SELECT 2",
            ][..],
            // SET statements combined in one Query ('Q') message
            &[
                "SET otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'; SET otel.tracestate = 'rojo=1'",
                "SELECT 1",
                "SELECT 2",
            ][..],
            // Reversed SET statements
            &[
                "SET otel.tracestate = 'rojo=1'",
                "SET otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'",
                "SELECT 1",
                "SELECT 2",
            ][..],
            &[
                "SET otel.tracestate = 'rojo=1'; SET otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'",
                "SELECT 1",
                "SELECT 2",
            ][..],
        ] {
            scoped_trace!("{sqls:?}");

            harness.enable_tracing()?;
            for sql in sqls {
                harness.simple_query(sql)?;
            }

            let all_spans = harness.collect_spans_count(2, time::Duration::from_secs(2));

            let first_span = all_spans.first().or_fail()?;
            verify_that!(
                all_spans,
                elements_are![
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&vec![0x11u8; 16]),
                        parent_span_id: eq(&vec![0x22u8; 8]),
                        trace_state: eq("rojo=1"),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.operation.name",
                            "SELECT"
                        ))),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: not(eq(&vec![0x11u8; 16])),
                        span_id: not(eq(&first_span.span_id)),
                        parent_span_id: is_empty(),
                        trace_state: eq(""),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.operation.name",
                            "SELECT"
                        ))),
                        ..
                    }),
                ]
            )?;
        }
        Ok(())
    }

    #[pgrx::pg_test]
    fn multiple_queries_multiple_spans() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        for sqls in [
            &["SELECT 1", "SELECT 2"][..], // Two Query ('Q') messages
        ] {
            scoped_trace!("{sqls:?}");

            harness.enable_tracing()?;
            for sql in sqls {
                harness.simple_query(sql)?;
            }

            let all_spans = harness.collect_spans_count(2, time::Duration::from_secs(2));

            let first_span = all_spans.first().or_fail()?;
            verify_that!(
                all_spans,
                elements_are![
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: not(is_empty()),
                        span_id: not(is_empty()),
                        parent_span_id: is_empty(),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.operation.name",
                            "SELECT"
                        ))),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: not(eq(&first_span.trace_id)),
                        span_id: not(eq(&first_span.span_id)),
                        parent_span_id: is_empty(),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.operation.name",
                            "SELECT"
                        ))),
                        ..
                    }),
                ]
            )?;
        }
        Ok(())
    }

    #[pgrx::pg_test]
    fn parallel_query_child_spans() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.simple_query(cfg_select! {
            any(feature = "pg15") => {
                "SET force_parallel_mode = regress; SET max_parallel_workers_per_gather = 1"
            }
            _ => {
                "SET debug_parallel_query = regress; SET max_parallel_workers_per_gather = 1"
            }
        })?;

        harness.simple_query("CREATE TABLE t_par AS SELECT generate_series(1, 1000) AS id")?;
        harness.simple_query("SET otel.trace_parallel_workers = on")?;
        harness.enable_tracing()?;
        harness.simple_query("SELECT count(*) FROM t_par")?;

        let all_spans = harness.collect_spans_count(2, time::Duration::from_secs(2));

        verify_that!(all_spans, len(eq(2)))?;

        let leader_span = all_spans
            .iter()
            .find(|s| s.parent_span_id.is_empty())
            .or_fail()?;
        let worker_span = all_spans
            .iter()
            .find(|s| !s.parent_span_id.is_empty())
            .or_fail()?;

        // Verify leader span
        verify_that!(leader_span.name, eq("SELECT"))?;
        verify_that!(leader_span.kind, eq(SpanKind::Server as i32))?;
        verify_that!(leader_span.status, is_empty())?;
        verify_that!(
            leader_span.attributes,
            contains(eq(&KeyValue::new_string("db.operation.name", "SELECT")))
        )?;

        // Verify worker span
        verify_that!(worker_span.name, eq("SELECT"))?;
        verify_that!(worker_span.kind, eq(SpanKind::Internal as i32))?;
        verify_that!(worker_span.status, is_empty())?;
        verify_that!(worker_span.trace_id, eq(&leader_span.trace_id))?;
        verify_that!(worker_span.parent_span_id, eq(&leader_span.span_id))?;
        verify_that!(worker_span.span_id, not(eq(&leader_span.span_id)))?;

        Ok(())
    }

    #[pgrx::pg_test]
    fn parallel_query_child_spans_include_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.simple_query(cfg_select! {
            any(feature = "pg15") => {
                "SET force_parallel_mode = regress; SET max_parallel_workers_per_gather = 1"
            }
            _ => {
                "SET debug_parallel_query = regress; SET max_parallel_workers_per_gather = 1"
            }
        })?;

        harness.simple_query("CREATE TABLE t_par_ctx AS SELECT generate_series(1, 1000) AS id")?;
        harness.simple_query("SET otel.trace_parallel_workers = on")?;
        harness.enable_tracing()?;

        for sqls in [
            &[
                "SELECT count(*) FROM t_par_ctx /* traceparent='00-11111111111111111111111111111111-2222222222222222-01',tracestate='rojo=1' */",
            ][..],
            &[
                "SET otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'",
                "SET otel.tracestate = 'rojo=1'",
                "SELECT count(*) FROM t_par_ctx",
            ][..],
        ] {
            for sql in sqls {
                harness.simple_query(sql)?;
            }

            let all_spans = harness.collect_spans_count(2, time::Duration::from_secs(2));

            let leader_span = all_spans
                .iter()
                .find(|s| s.kind == SpanKind::Server as i32)
                .or_fail()?;

            verify_that!(
                all_spans,
                unordered_elements_are![
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&vec![0x11u8; 16]),
                        parent_span_id: eq(&vec![0x22u8; 8]),
                        trace_state: eq("rojo=1"),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.operation.name",
                            "SELECT"
                        ))),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Internal as i32)),
                        status: is_empty(),
                        trace_id: eq(&vec![0x11u8; 16]),
                        span_id: not(eq(&leader_span.span_id)),
                        parent_span_id: eq(&leader_span.span_id),
                        trace_state: eq("rojo=1"),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.operation.name",
                            "SELECT"
                        ))),
                        ..
                    }),
                ]
            )?;
        }

        Ok(())
    }

    #[pgrx::pg_test]
    fn trace_target_matrix() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.simple_query(cfg_select! {
            any(feature = "pg15") => {
                "SET force_parallel_mode = regress; SET max_parallel_workers_per_gather = 1"
            }
            _ => {
                "SET debug_parallel_query = regress; SET max_parallel_workers_per_gather = 1"
            }
        })?;
        harness.simple_query("CREATE TABLE t_matrix AS SELECT generate_series(1, 1000) AS id")?;

        #[rustfmt::skip]
        let cases = [
            // (export, set (txns,  stmts, parallel), has (txns,  stmts, parallel))
            (   false,      (true,  true,  true ),        (false, false, false)   ), // Main off, targets on
            (   true,       (false, false, false),        (false, false, false)   ), // Main on, targets off

            (   true,       (true,  false, false),        (true,  false, false)   ), // Transactions only
            (   true,       (false, true,  false),        (false, true,  false)   ), // Statements only
            (   true,       (true,  false, true ),        (true,  false, false)   ), // Parallel needs statements
            (   true,       (false, true,  true ),        (false, true,  true )   ), // Transactions off

            (   true,       (true,  true,  false),        (true,  true,  false)   ), // Defaults
            (   true,       (true,  true,  true ),        (true,  true,  true )   ), // Full visibility
        ];

        for (export, (txns, stmts, parallel), (has_txn, has_stmt, has_parallel)) in cases {
            harness.simple_query(&format!(
                "SET otel.export_traces = {export}; \
                 SET otel.trace_transactions = {txns}; \
                 SET otel.trace_statements = {stmts}; \
                 SET otel.trace_parallel_workers = {parallel};"
            ))?;

            harness.simple_query("BEGIN; SELECT count(*) FROM t_matrix; COMMIT;")?;

            let spans = harness.collect_spans(time::Duration::from_millis(200));

            let txn_count = spans.iter().filter(|s| s.name == "TRANSACTION").count();
            let leader_count = spans
                .iter()
                .filter(|s| s.name == "SELECT" && s.kind == SpanKind::Server as i32)
                .count();
            let parallel_count = spans
                .iter()
                .filter(|s| s.name == "SELECT" && s.kind == SpanKind::Internal as i32)
                .count();
            let commit_count = spans.iter().filter(|s| s.name == "COMMIT").count();

            verify_that!(txn_count, eq(if has_txn { 1 } else { 0 }))?;
            verify_that!(leader_count, eq(if has_stmt { 1 } else { 0 }))?;
            verify_that!(commit_count, eq(if has_stmt { 1 } else { 0 }))?;
            verify_that!(parallel_count, eq(if has_parallel { 1 } else { 0 }))?;
        }

        Ok(())
    }

    #[pgrx::pg_test]
    fn sqlcommenter_disabled() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.simple_query("SET otel.sqlcommenter = off")?;
        harness.enable_tracing()?;
        harness.simple_query(
            "SELECT 1 /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */;",
        )?;

        let spans = harness.collect_spans(time::Duration::from_millis(100));
        verify_that!(spans, len(eq(1)))?;
        let span = &spans[0];
        verify_that!(span.trace_id, not(eq(&vec![0x11u8; 16])))?;
        verify_that!(span.parent_span_id, is_empty())
    }

    #[pgrx::pg_test]
    fn log_correlation_without_tracing() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_telemetry("logs")?;
        harness.simple_query("SET otel.export_traces = off; SET otel.export_logs = on;")?;

        harness.simple_query(
            "DO $$ BEGIN RAISE WARNING 'warning without trace export'; END $$ /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */;",
        )?;

        let logs = harness.collect_logs_count(1, time::Duration::from_secs(2));
        let warning_log = logs
            .iter()
            .find(|l| l.severity_text == "WARNING")
            .or_fail()?;
        verify_that!(warning_log.trace_id, eq(&vec![0x11u8; 16]))?;
        verify_that!(warning_log.span_id, eq(&vec![0x22u8; 8]))?;

        // Verify no trace spans were emitted
        let spans = harness.collect_spans(time::Duration::from_millis(100));
        verify_that!(spans, is_empty())
    }
}
