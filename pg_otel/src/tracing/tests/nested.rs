// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn nested_default_budget_emits_only_top_level_span() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.client.batch_execute(
            "CREATE OR REPLACE PROCEDURE test_proc_default() LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM 1;
                PERFORM 2;
            END;
            $$;",
        )?;

        harness.enable_tracing()?;
        harness.client.batch_execute("CALL test_proc_default();")?;

        let all_spans = harness.collect_spans(time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("CALL"))?;
        verify_that!(all_spans[0].kind, eq(SpanKind::Server as i32))?;
        verify_that!(all_spans[0].parent_span_id, is_empty())
    }

    #[pgrx::pg_test]
    fn nested_opt_in_emits_internal_child_spans() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.client.batch_execute(
            "CREATE TEMP TABLE test_items (id int);
            CREATE OR REPLACE PROCEDURE test_proc_opt_in() LANGUAGE plpgsql AS $$
            BEGIN
                INSERT INTO test_items VALUES (1);
                PERFORM count(*) FROM test_items;
            END;
            $$;",
        )?;

        harness.enable_tracing()?;
        harness.client.batch_execute(
            "SET LOCAL otel.trace_nested_statements = 10;
            CALL test_proc_opt_in();",
        )?;

        let all_spans = harness.collect_spans_count(3, time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(3)))?;

        // Root span is CALL
        let root = all_spans.iter().find(|s| s.name == "CALL").or_fail()?;
        verify_that!(root.kind, eq(SpanKind::Server as i32))?;
        verify_that!(root.parent_span_id, is_empty())?;

        // Child spans are INSERT and SELECT
        let children: Vec<&Span> = all_spans.iter().filter(|s| s.name != "CALL").collect();
        verify_that!(children, len(eq(2)))?;

        for child in children {
            verify_that!(child.kind, eq(SpanKind::Internal as i32))?;
            verify_that!(child.trace_id, eq(&root.trace_id))?;
            verify_that!(child.parent_span_id, eq(&root.span_id))?;
        }

        Ok(())
    }

    #[pgrx::pg_test]
    fn caller_governance_prevents_callee_self_activation() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.client.batch_execute(
            "CREATE OR REPLACE PROCEDURE test_proc_governed() LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM 1;
                PERFORM 2;
            END;
            $$;
            ALTER PROCEDURE test_proc_governed() SET otel.trace_nested_statements = 10;",
        )?;

        harness.enable_tracing()?;
        // Caller has default otel.trace_nested_statements = 0
        harness.client.batch_execute("CALL test_proc_governed();")?;

        let all_spans = harness.collect_spans(time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("CALL"))?;
        verify_that!(all_spans[0].kind, eq(SpanKind::Server as i32))?;
        verify_that!(all_spans[0].parent_span_id, is_empty())
    }

    #[pgrx::pg_test]
    fn dba_kill_switch_suppresses_all_nested_spans() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.client.batch_execute(
            "CREATE OR REPLACE PROCEDURE test_proc_kill_switch() LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM 1;
                PERFORM 2;
            END;
            $$;",
        )?;

        harness.enable_tracing()?;
        harness.client.batch_execute(
            "SET otel.max_nested_statements = 0;
            SET LOCAL otel.trace_nested_statements = 10;
            CALL test_proc_kill_switch();
            SET otel.max_nested_statements = 50;",
        )?;

        let all_spans = harness.collect_spans(time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("CALL"))?;
        verify_that!(all_spans[0].kind, eq(SpanKind::Server as i32))
    }

    #[pgrx::pg_test]
    fn loop_bounding_and_dropped_counter() -> Result<()> {
        let mut harness = TracingHarness::new()?;
        harness.enable_tracing()?;

        // Allocate budget of 3 nested statements, but loop 10 times
        harness.client.batch_execute(
            "SET LOCAL otel.trace_nested_statements = 3;
            DO $$
            BEGIN
                FOR i IN 1..10 LOOP
                    PERFORM 1;
                END LOOP;
            END;
            $$;",
        )?;

        let all_spans = harness.collect_spans_count(4, time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(4)))?;

        let root = all_spans.iter().find(|s| s.name == "DO").or_fail()?;
        verify_that!(root.kind, eq(SpanKind::Server as i32))?;
        verify_that!(
            root.attributes,
            contains(eq(&KeyValue::new_int("otel.dropped_child_spans", 7i64)))
        )?;

        let children: Vec<&Span> = all_spans.iter().filter(|s| s.name != "DO").collect();
        verify_that!(children, len(eq(3)))?;
        for child in children {
            verify_that!(child.kind, eq(SpanKind::Internal as i32))?;
            verify_that!(child.parent_span_id, eq(&root.span_id))?;
        }

        Ok(())
    }

    #[pgrx::pg_test]
    fn callee_silencing_preserves_caller_budget() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.client.batch_execute(
            "CREATE OR REPLACE PROCEDURE inner_silenced() LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM 1;
                PERFORM 2;
                PERFORM 3;
            END;
            $$;
            ALTER PROCEDURE inner_silenced() SET otel.trace_nested_statements = 0;

            CREATE OR REPLACE PROCEDURE outer_proc() LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM 100;
                CALL inner_silenced();
                PERFORM 200;
            END;
            $$;",
        )?;

        harness.enable_tracing()?;
        harness.client.batch_execute(
            "SET LOCAL otel.trace_nested_statements = 10;
            CALL outer_proc();",
        )?;

        let all_spans = harness.collect_spans_count(4, time::Duration::from_secs(1));

        // Expected spans:
        // 1. CALL outer_proc() [Server]
        // 2. SELECT 100 [Internal]
        // 3. CALL inner_silenced() [Internal]
        // (statements inside inner_silenced are silenced, 0 spans)
        // 4. SELECT 200 [Internal]
        verify_that!(all_spans, len(eq(4)))?;

        let outer = all_spans
            .iter()
            .find(|s| s.name == "CALL" && s.kind == SpanKind::Server as i32)
            .or_fail()?;
        let inner_call = all_spans
            .iter()
            .find(|s| s.name == "CALL" && s.kind == SpanKind::Internal as i32)
            .or_fail()?;
        verify_that!(inner_call.parent_span_id, eq(&outer.span_id))?;

        let selects: Vec<&Span> = all_spans.iter().filter(|s| s.name == "SELECT").collect();
        verify_that!(selects, len(eq(2)))?;
        for s in selects {
            verify_that!(s.parent_span_id, eq(&outer.span_id))?;
        }

        Ok(())
    }

    #[pgrx::pg_test]
    fn trigger_execution_detects_trigger_attribute() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.client.batch_execute(
            "CREATE TEMP TABLE audit_log (msg text);
            CREATE TEMP TABLE main_table (id int);

            CREATE OR REPLACE FUNCTION trig_func() RETURNS trigger LANGUAGE plpgsql AS $$
            BEGIN
                INSERT INTO audit_log VALUES ('inserted ' || NEW.id);
                RETURN NEW;
            END;
            $$;

            CREATE TRIGGER after_insert_trig
            AFTER INSERT ON main_table
            FOR EACH ROW EXECUTE FUNCTION trig_func();",
        )?;

        harness.enable_tracing()?;
        harness.client.batch_execute(
            "SET LOCAL otel.trace_nested_statements = 5;
            INSERT INTO main_table VALUES (42);",
        )?;

        let all_spans = harness.collect_spans_count(2, time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(2)))?;

        let root = all_spans
            .iter()
            .find(|s| s.kind == SpanKind::Server as i32)
            .or_fail()?;
        verify_that!(root.name, eq("INSERT"))?;

        let trigger_span = all_spans
            .iter()
            .find(|s| s.kind == SpanKind::Internal as i32)
            .or_fail()?;
        verify_that!(trigger_span.name, eq("INSERT"))?;
        verify_that!(trigger_span.parent_span_id, eq(&root.span_id))?;
        verify_that!(
            trigger_span.attributes,
            contains(eq(&KeyValue::new_bool("db.postgresql.trigger", true)))
        )
    }

    #[pgrx::pg_test]
    fn nested_notice_correlates_with_active_span() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.client.batch_execute(
            "CREATE OR REPLACE PROCEDURE proc_with_notice() LANGUAGE plpgsql AS $$
            BEGIN
                RAISE NOTICE 'notice from nested procedure';
            END;
            $$;
            SET log_statement = 'none';
            SET log_min_messages = 'notice';",
        )?;

        harness.enable_telemetry("traces,logs")?;
        harness.client.batch_execute(
            "SET LOCAL otel.trace_nested_statements = 5;
            CALL proc_with_notice();",
        )?;

        let all_spans = harness.collect_spans_count(1, time::Duration::from_secs(1));
        verify_that!(all_spans, len(eq(1)))?;
        let proc_span = &all_spans[0];
        verify_that!(proc_span.name, eq("CALL"))?;

        let logs = harness.collect_logs_count(1, time::Duration::from_secs(1));
        verify_that!(logs, len(eq(1)))?;
        let notice_log = &logs[0];

        verify_that!(
            notice_log.body,
            eq(&Some(AnyValue::new_string("notice from nested procedure")))
        )?;
        verify_that!(notice_log.trace_id, eq(&proc_span.trace_id))?;
        verify_that!(notice_log.span_id, eq(&proc_span.span_id))?;

        Ok(())
    }
}
