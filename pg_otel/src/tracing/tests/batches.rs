// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn multi_statement_batch() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.client.batch_execute("SELECT 1; SELECT 2")?;

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
                    name: eq("TRANSACTION"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    parent_span_id: is_empty(),
                    attributes: contains(eq(&KeyValue::new_string(
                        "db.operation.name",
                        "TRANSACTION"
                    ))),
                    ..
                }),
            ]
        )?;
        Ok(())
    }

    #[pgrx::pg_test]
    fn multi_statement_batch_error() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        verify_that!(
            harness
                .client
                .batch_execute("SELECT 1; SELECT 1/0; SELECT 2"),
            err(anything())
        )?;

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
                    attributes: contains(eq(&KeyValue::new_string("db.operation.name", "SELECT"))),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: some(eq(&SpanStatus::error(""))),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    attributes: contains(eq(&KeyValue::new_string(
                        "db.response.status_code",
                        "22012"
                    ))),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: some(eq(&SpanStatus::error(""))),
                    parent_span_id: is_empty(),
                    attributes: contains(eq(&KeyValue::new_string(
                        "db.response.status_code",
                        "22012"
                    ))),
                    ..
                }),
            ]
        )?;
        Ok(())
    }

    #[pgrx::pg_test]
    fn multi_statement_batch_with_mid_batch_begin() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness
            .client
            .batch_execute("SELECT 1; BEGIN; SELECT 2; COMMIT")?;

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
        Ok(())
    }

    #[pgrx::pg_test]
    fn multi_statement_batch_with_mid_batch_commit() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.client.batch_execute("SELECT 1; COMMIT; SELECT 2")?;

        let all_spans = harness.collect_spans_count(5, time::Duration::from_secs(2));

        verify_that!(all_spans, len(eq(5)))?;
        let first_xact = &all_spans[2];
        let second_xact = &all_spans[4];
        verify_that!(first_xact.name, eq("TRANSACTION"))?;
        verify_that!(second_xact.name, eq("TRANSACTION"))?;
        verify_that!(first_xact.trace_id, not(eq(&second_xact.trace_id)))?;

        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&first_xact.trace_id),
                    parent_span_id: eq(&first_xact.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    trace_id: eq(&first_xact.trace_id),
                    parent_span_id: eq(&first_xact.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    trace_id: eq(&first_xact.trace_id),
                    parent_span_id: is_empty(),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    trace_id: eq(&second_xact.trace_id),
                    parent_span_id: eq(&second_xact.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("TRANSACTION"),
                    trace_id: eq(&second_xact.trace_id),
                    parent_span_id: is_empty(),
                    ..
                }),
            ]
        )?;
        Ok(())
    }

    #[pgrx::pg_test]
    fn multi_statement_batch_with_comment_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.client.batch_execute(
            "SELECT 1 /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */; SELECT 2",
        )?;

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
                    name: eq("SELECT"),
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
        )?;
        Ok(())
    }
}
