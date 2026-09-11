// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn simple_query_error() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        for sql in [
            // This fails while planning, when Postgres folds constants.
            "SELECT 1/0",
            // This fails while running, when Postgres produces rows.
            "WITH t(x) AS MATERIALIZED (SELECT 0) SELECT 1/x FROM t",
        ] {
            scoped_trace!("{sql:?}");

            harness.enable_tracing()?;
            verify_that!(harness.client.batch_execute(sql), err(anything()))?;

            let all_spans = harness
                .collect_spans_until(time::Duration::from_secs(2), |spans, _| !spans.is_empty());

            verify_that!(
                all_spans,
                elements_are![matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: some(eq(&SpanStatus::error(""))),
                    trace_id: not(is_empty()),
                    parent_span_id: is_empty(),
                    span_id: not(is_empty()),
                    attributes: container_eq([
                        KeyValue::new_string("db.operation.name", "SELECT"),
                        KeyValue::new_string("db.system.name", "postgresql"),
                        KeyValue::new_string("db.response.status_code", "22012"),
                    ]),
                    ..
                })]
            )?;
        }
        Ok(())
    }

    #[pgrx::pg_test]
    fn query_error_in_transaction() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        for sql in [
            // This fails while planning, when Postgres folds constants.
            "SELECT 1/0",
            // This fails while running, when Postgres produces rows.
            "WITH t(x) AS MATERIALIZED (SELECT 0) SELECT 1/x FROM t",
        ] {
            scoped_trace!("{sql:?}");

            harness.enable_tracing()?;
            harness.client.batch_execute("BEGIN")?;
            verify_that!(harness.client.batch_execute(sql), err(anything()))?;
            harness.client.batch_execute("ROLLBACK")?;

            let all_spans = harness.collect_transaction_spans(time::Duration::from_secs(2));

            let xact_span = all_spans.last().or_fail()?;
            verify_that!(
                all_spans,
                elements_are![
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: some(eq(&SpanStatus::error(""))),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.response.status_code",
                            "22012",
                        ))),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("ROLLBACK"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: eq(&xact_span.trace_id),
                        parent_span_id: eq(&xact_span.span_id),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("TRANSACTION"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: some(eq(&SpanStatus::error(""))),
                        trace_id: not(is_empty()),
                        parent_span_id: is_empty(),
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
        let mut harness = TracingHarness::new()?;

        harness
            .client
            .batch_execute("CREATE TEMP TABLE p (id int PRIMARY KEY)")?;
        harness.client.batch_execute(
            "CREATE TEMP TABLE c (p_id int REFERENCES p(id) DEFERRABLE INITIALLY DEFERRED)",
        )?;

        harness.enable_tracing()?;
        harness.client.batch_execute("BEGIN")?;
        harness.client.batch_execute("INSERT INTO c VALUES (999)")?;
        verify_that!(harness.client.batch_execute("COMMIT"), err(anything()))?;

        let all_spans = harness.collect_transaction_spans(time::Duration::from_secs(2));

        let xact_span = all_spans.last().or_fail()?;
        verify_that!(
            all_spans,
            elements_are![
                matches_pattern!(Span {
                    name: eq("INSERT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("COMMIT"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: some(eq(&SpanStatus::error(""))),
                    trace_id: eq(&xact_span.trace_id),
                    parent_span_id: eq(&xact_span.span_id),
                    attributes: contains(eq(&KeyValue::new_string(
                        "db.response.status_code",
                        "23503",
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
                        "23503",
                    ))),
                    ..
                }),
            ]
        )
    }
}
