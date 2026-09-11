// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn single_query_in_transaction() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        for sqls in [
            &["BEGIN", "SELECT 1", "COMMIT"][..], // Three Query ('Q') messages
            &["BEGIN; SELECT 1; COMMIT"][..],     // One Query ('Q') message with three statements
        ] {
            scoped_trace!("{sqls:?}");

            harness.enable_tracing()?;
            for sql in sqls {
                harness.client.batch_execute(sql)?;
            }

            let all_spans = harness.collect_transaction_spans(time::Duration::from_secs(2));

            let xact_span = all_spans.last().or_fail()?;
            verify_that!(
                all_spans,
                elements_are![
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("COMMIT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("TRANSACTION"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        parent_span_id: is_empty(),
                        ..
                    }),
                ]
            )?;
        }
        Ok(())
    }

    #[pgrx::pg_test]
    fn multiple_queries_in_transaction() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        for sqls in [
            &["BEGIN", "SELECT 1", "SELECT 2", "COMMIT"][..], // Four Query ('Q') messages
            &["BEGIN; SELECT 1; SELECT 2; COMMIT"][..], // One Query ('Q') message with four statements
        ] {
            scoped_trace!("{sqls:?}");

            harness.enable_tracing()?;
            for sql in sqls {
                harness.client.batch_execute(sql)?;
            }

            let all_spans = harness.collect_transaction_spans(time::Duration::from_secs(2));

            let xact_span = all_spans.last().or_fail()?;
            verify_that!(
                all_spans,
                elements_are![
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("COMMIT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("TRANSACTION"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        parent_span_id: is_empty(),
                        ..
                    }),
                ]
            )?;
        }
        Ok(())
    }

    #[pgrx::pg_test]
    fn single_query_in_transaction_with_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        for sqls in [
            // Three Query ('Q') messages
            &[
                "BEGIN /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */",
                "SELECT 1",
                "COMMIT",
            ][..],
            // One Query ('Q') message with three statements
            &[
                "BEGIN /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */; SELECT 1; COMMIT",
            ][..],
        ] {
            scoped_trace!("{sqls:?}");

            harness.enable_tracing()?;
            for sql in sqls {
                harness.client.batch_execute(sql)?;
            }

            let all_spans = harness.collect_transaction_spans(time::Duration::from_secs(2));

            let xact_span = all_spans.last().or_fail()?;
            verify_that!(
                all_spans,
                elements_are![
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&vec![0x11; 16]),
                        parent_span_id: eq(&xact_span.span_id),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("COMMIT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&vec![0x11; 16]),
                        parent_span_id: eq(&xact_span.span_id),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("TRANSACTION"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&vec![0x11; 16]),
                        parent_span_id: eq(&vec![0x22; 8]),
                        ..
                    }),
                ]
            )?;
        }
        Ok(())
    }

    #[pgrx::pg_test]
    fn single_query_with_context_in_transaction() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.client.batch_execute("BEGIN")?;
        harness.client.batch_execute(
            "SELECT 1 /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */",
        )?;
        harness.client.batch_execute("COMMIT")?;

        let all_spans = harness.collect_transaction_spans(time::Duration::from_secs(2));

        let xact_span = all_spans.last().or_fail()?;
        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x11; 16]),
                    parent_span_id: eq(&vec![0x22; 8]),
                    links: elements_are![matches_pattern!(Link {
                        trace_id: eq(&xact_span.trace_id),
                        span_id: eq(&xact_span.span_id),
                        ..
                    })],
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: not(eq(&vec![0x11; 16])),
                    parent_span_id: is_empty(),
                    ..
                }),
            ]
        )
    }

    #[pgrx::pg_test]
    fn parent_override_in_transaction_with_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.client.batch_execute(
            "BEGIN /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */",
        )?;
        harness.client.batch_execute(
            "SELECT 1 /* traceparent='00-33333333333333333333333333333333-4444444444444444-01' */",
        )?;
        harness.client.batch_execute("SELECT 2")?;
        harness.client.batch_execute("COMMIT")?;

        let all_spans = harness.collect_transaction_spans(time::Duration::from_secs(2));

        let xact_span = all_spans.last().or_fail()?;
        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x33; 16]),
                    parent_span_id: eq(&vec![0x44; 8]),
                    links: elements_are![matches_pattern!(Link {
                        trace_id: eq(&xact_span.trace_id),
                        span_id: eq(&xact_span.span_id),
                        ..
                    })],
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x11; 16]),
                    parent_span_id: eq(&vec![0x22; 8]),
                    ..
                }),
            ]
        )
    }

    #[pgrx::pg_test]
    fn transaction_with_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        for sqls in [
            // A connection pooler might use GUC *before* the transaction
            &[
                "SET pg_otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'",
                "BEGIN",
                "SELECT 1",
                "COMMIT",
                "SELECT 2",
            ][..],
            &[
                "SET pg_otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'",
                "BEGIN; SELECT 1; COMMIT",
                "SELECT 2",
            ][..],
            // A client that allows properties on a transaction might use GUC *in* the transaction
            &[
                "BEGIN",
                "SET LOCAL pg_otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'",
                "SELECT 1",
                "COMMIT",
                "SELECT 2",
            ][..],
            &[
                "BEGIN; SET LOCAL pg_otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'; SELECT 1; COMMIT",
                "SELECT 2",
            ][..],
        ] {
            scoped_trace!("{sqls:?}");

            harness.enable_tracing()?;
            for sql in sqls {
                harness.client.batch_execute(sql)?;
            }

            let all_spans = harness.collect_spans_count(4, time::Duration::from_secs(2));

            let xact_span = all_spans
                .iter()
                .find(|s| s.name == "TRANSACTION")
                .or_fail()?;

            verify_that!(
                all_spans,
                elements_are![
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.operation.name",
                            "SELECT"
                        ))),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("COMMIT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.operation.name",
                            "COMMIT"
                        ))),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("TRANSACTION"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&vec![0x11u8; 16]),
                        parent_span_id: eq(&vec![0x22u8; 8]),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.operation.name",
                            "TRANSACTION"
                        ))),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: not(eq(&xact_span.trace_id)),
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
    fn parent_override_in_transaction_with_set_local_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.client.batch_execute(
            "BEGIN /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */",
        )?;
        harness.client.batch_execute(
            "SET LOCAL pg_otel.traceparent = '00-33333333333333333333333333333333-4444444444444444-01'",
        )?;
        harness.client.batch_execute("SELECT 1")?;
        harness.client.batch_execute("SELECT 2")?;
        harness.client.batch_execute("COMMIT")?;

        let all_spans = harness.collect_transaction_spans(time::Duration::from_secs(2));

        let xact_span = all_spans.last().or_fail()?;
        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x33; 16]),
                    parent_span_id: eq(&vec![0x44; 8]),
                    links: elements_are![matches_pattern!(Link {
                        trace_id: eq(&xact_span.trace_id),
                        span_id: eq(&xact_span.span_id),
                        ..
                    })],
                    attributes: contains(eq(&KeyValue::new_string("db.operation.name", "SELECT"))),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    attributes: contains(eq(&KeyValue::new_string("db.operation.name", "SELECT"))),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    attributes: contains(eq(&KeyValue::new_string("db.operation.name", "COMMIT"))),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x11; 16]),
                    parent_span_id: eq(&vec![0x22; 8]),
                    attributes: contains(eq(&KeyValue::new_string(
                        "db.operation.name",
                        "TRANSACTION"
                    ))),
                    ..
                }),
            ]
        )
    }
}
