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
        harness.client.batch_execute("SELECT 1")?;

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
    fn query_with_comment_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.client.batch_execute(
            "SELECT 1 /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */",
        )?;

        let all_spans = harness.collect_spans(time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("SELECT"))?;
        verify_that!(all_spans[0].kind, eq(SpanKind::Server as i32))?;
        verify_that!(all_spans[0].status, is_empty())?;
        verify_that!(all_spans[0].trace_id, eq(&vec![0x11u8; 16]))?;
        verify_that!(all_spans[0].parent_span_id, eq(&vec![0x22u8; 8]))?;
        verify_that!(
            all_spans[0].attributes,
            contains(eq(&KeyValue::new_string("db.operation.name", "SELECT")))
        )
    }

    #[pgrx::pg_test]
    fn query_with_guc_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        for sqls in [
            // Three Query ('Q') messages
            &[
                "SET pg_otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'",
                "SELECT 1",
                "SELECT 2",
            ][..],
            // One Query ('Q') message with three statements
            &[
                "SET pg_otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'; SELECT 1; SELECT 2",
            ][..],
        ] {
            scoped_trace!("{sqls:?}");

            harness.enable_tracing()?;
            for sql in sqls {
                harness.client.batch_execute(sql)?;
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
            &["SELECT 1; SELECT 2"][..],   // One Query ('Q') message with two statements
        ] {
            scoped_trace!("{sqls:?}");

            harness.enable_tracing()?;
            for sql in sqls {
                harness.client.batch_execute(sql)?;
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

        harness.client.batch_execute(cfg_select! {
            any(feature = "pg15") => {
                "SET force_parallel_mode = regress; SET max_parallel_workers_per_gather = 1"
            }
            _ => {
                "SET debug_parallel_query = regress; SET max_parallel_workers_per_gather = 1"
            }
        })?;

        harness
            .client
            .batch_execute("CREATE TABLE t_par AS SELECT generate_series(1, 1000) AS id")?;
        harness.enable_tracing()?;
        harness.client.batch_execute("SELECT count(*) FROM t_par")?;

        let all_spans = harness.collect_spans_count(2, time::Duration::from_secs(2));

        let _ = harness.client.batch_execute("DROP TABLE t_par");

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
        verify_that!(worker_span.kind, eq(SpanKind::Server as i32))?;
        verify_that!(worker_span.status, is_empty())?;
        verify_that!(worker_span.trace_id, eq(&leader_span.trace_id))?;
        verify_that!(worker_span.parent_span_id, eq(&leader_span.span_id))?;
        verify_that!(worker_span.span_id, not(eq(&leader_span.span_id)))?;

        Ok(())
    }
}
