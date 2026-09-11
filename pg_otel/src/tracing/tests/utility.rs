// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn simple_utility_single_span() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness
            .client
            .batch_execute("CREATE TEMP TABLE t1 (id int)")?;

        let all_spans = harness.collect_spans(time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("CREATE"))?;
        verify_that!(all_spans[0].kind, eq(SpanKind::Server as i32))?;
        verify_that!(all_spans[0].status, is_empty())?;
        verify_that!(all_spans[0].parent_span_id, is_empty())?;
        verify_that!(
            all_spans[0].attributes,
            contains(eq(&KeyValue::new_string("db.operation.name", "CREATE")))
        )
    }

    #[pgrx::pg_test]
    fn utility_with_context() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;
        harness.client.batch_execute(
            "CREATE TEMP TABLE t2 (id int) \
                /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */",
        )?;

        let all_spans = harness.collect_spans(time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("CREATE"))?;
        verify_that!(all_spans[0].kind, eq(SpanKind::Server as i32))?;
        verify_that!(all_spans[0].status, is_empty())?;
        verify_that!(all_spans[0].trace_id, eq(&vec![0x11u8; 16]))?;
        verify_that!(all_spans[0].parent_span_id, eq(&vec![0x22u8; 8]))?;
        verify_that!(
            all_spans[0].attributes,
            contains(eq(&KeyValue::new_string("db.operation.name", "CREATE")))
        )
    }
}
