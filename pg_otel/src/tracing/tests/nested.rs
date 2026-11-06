// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use super::*;
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn nested_default_budget_emits_only_top_level_span() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.simple_query(
            "CREATE OR REPLACE PROCEDURE test_proc_default() LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM 1;
                PERFORM 2;
            END;
            $$;",
        )?;

        harness.enable_tracing()?;
        harness.simple_query("CALL test_proc_default();")?;

        let all_spans = harness.collect_spans(time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("CALL"))?;
        verify_that!(all_spans[0].kind, eq(SpanKind::Server as i32))?;
        verify_that!(all_spans[0].parent_span_id, is_empty())
    }

    #[pgrx::pg_test]
    fn nested_opt_in_emits_internal_child_spans() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.simple_query(
            "CREATE TEMP TABLE test_items (id int);
            CREATE OR REPLACE PROCEDURE test_proc_opt_in() LANGUAGE plpgsql AS $$
            BEGIN
                INSERT INTO test_items VALUES (1);
                PERFORM count(*) FROM test_items;
            END;
            $$;",
        )?;

        harness.enable_tracing()?;
        harness.simple_query("SET otel.trace_nested_statements = 10")?;
        harness.simple_query("CALL test_proc_opt_in()")?;

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

        harness.simple_query(
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
        harness.simple_query("CALL test_proc_governed();")?;

        let all_spans = harness.collect_spans(time::Duration::from_secs(1));

        verify_that!(all_spans, len(eq(1)))?;
        verify_that!(all_spans[0].name, eq("CALL"))?;
        verify_that!(all_spans[0].kind, eq(SpanKind::Server as i32))?;
        verify_that!(all_spans[0].parent_span_id, is_empty())
    }

    #[pgrx::pg_test]
    fn dba_kill_switch_suppresses_all_nested_spans() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.simple_query(
            "CREATE OR REPLACE PROCEDURE test_proc_kill_switch() LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM 1;
                PERFORM 2;
            END;
            $$;",
        )?;

        harness.enable_tracing()?;
        harness.simple_query("SET otel.max_nested_statements = 0")?;
        harness.simple_query("SET otel.trace_nested_statements = 10")?;
        harness.simple_query("CALL test_proc_kill_switch()")?;
        harness.simple_query("SET otel.max_nested_statements = 50")?;

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
        harness.simple_query("SET otel.trace_nested_statements = 3;")?;
        harness.simple_query(
            "DO $$
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

        harness.simple_query(
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
        harness.simple_query("SET otel.trace_nested_statements = 10")?;
        harness.simple_query("CALL outer_proc()")?;

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

        harness.simple_query(
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
        harness.simple_query("SET otel.trace_nested_statements = 5")?;
        harness.simple_query("INSERT INTO main_table VALUES (42)")?;

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

        harness.simple_query(
            "CREATE OR REPLACE PROCEDURE proc_with_notice() LANGUAGE plpgsql AS $$
            BEGIN
                RAISE NOTICE 'notice from nested procedure';
            END;
            $$;
            SET log_statement = 'none';
            SET log_min_messages = 'notice';",
        )?;

        harness.enable_telemetry("traces,logs")?;
        harness.simple_query("SET otel.trace_nested_statements = 5")?;
        harness.simple_query("CALL proc_with_notice()")?;

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

    #[pgrx::pg_test]
    fn nested_parallel_query_child_spans() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.simple_query(cfg_select! {
            any(feature = "pg15") => {
                "SET force_parallel_mode = regress; SET max_parallel_workers_per_gather = 1"
            }
            _ => {
                "SET debug_parallel_query = regress; SET max_parallel_workers_per_gather = 1"
            }
        })?;

        harness.simple_query(
            "CREATE TABLE t_par_nested AS SELECT generate_series(1, 1000) AS id;
            CREATE OR REPLACE PROCEDURE proc_parallel() LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM count(*) FROM t_par_nested;
            END;
            $$;",
        )?;

        harness.enable_tracing()?;

        // Case 1: With nested statement budget enabled (CALL -> nested SELECT -> parallel worker SELECT)
        harness.simple_query("SET otel.trace_nested_statements = 10")?;
        harness.simple_query(
            "CALL proc_parallel() /* traceparent='00-11111111111111111111111111111111-2222222222222222-01',tracestate='rojo=1' */",
        )?;

        let all_spans = harness.collect_spans_count(3, time::Duration::from_secs(2));
        verify_that!(all_spans, len(eq(3)))?;

        let call_span = all_spans.iter().find(|s| s.name == "CALL").or_fail()?;
        let nested_select_span = all_spans
            .iter()
            .find(|s| s.name == "SELECT" && s.parent_span_id == call_span.span_id)
            .or_fail()?;

        verify_that!(
            all_spans,
            unordered_elements_are![
                matches_pattern!(Span {
                    name: eq("CALL"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x11u8; 16]),
                    parent_span_id: eq(&vec![0x22u8; 8]),
                    trace_state: eq("rojo=1"),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Internal as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x11u8; 16]),
                    span_id: eq(&nested_select_span.span_id),
                    parent_span_id: eq(&call_span.span_id),
                    trace_state: eq("rojo=1"),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Internal as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x11u8; 16]),
                    span_id: not(eq(&nested_select_span.span_id)),
                    parent_span_id: eq(&nested_select_span.span_id),
                    trace_state: eq("rojo=1"),
                    ..
                }),
            ]
        )?;

        // Case 2: With nested statement budget = 0 (CALL -> parallel worker SELECT)
        harness.simple_query("SET otel.trace_nested_statements = 0")?;
        harness.simple_query(
            "CALL proc_parallel() /* traceparent='00-33333333333333333333333333333333-4444444444444444-01',tracestate='congo=2' */",
        )?;

        let all_spans = harness.collect_spans_count(2, time::Duration::from_secs(2));
        verify_that!(all_spans, len(eq(2)))?;

        let call_span = all_spans.iter().find(|s| s.name == "CALL").or_fail()?;

        verify_that!(
            all_spans,
            unordered_elements_are![
                matches_pattern!(Span {
                    name: eq("CALL"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x33u8; 16]),
                    parent_span_id: eq(&vec![0x44u8; 8]),
                    trace_state: eq("congo=2"),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Internal as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x33u8; 16]),
                    span_id: not(eq(&call_span.span_id)),
                    parent_span_id: eq(&call_span.span_id),
                    trace_state: eq("congo=2"),
                    ..
                }),
            ]
        )?;

        Ok(())
    }

    #[pgrx::pg_test]
    fn nested_context_is_immutable() -> Result<()> {
        let mut harness = TracingHarness::new()?;

        harness.simple_query(
            "CREATE OR REPLACE PROCEDURE proc_set_traceparent() LANGUAGE plpgsql AS $$
            BEGIN
                SET otel.traceparent = '00-33333333333333333333333333333333-4444444444444444-01';
            END;
            $$;
            CREATE OR REPLACE PROCEDURE proc_set_tracestate() LANGUAGE plpgsql AS $$
            BEGIN
                SET otel.tracestate = 'congo=2';
            END;
            $$;
            CREATE OR REPLACE PROCEDURE proc_comment_override() LANGUAGE plpgsql AS $$
            BEGIN
                PERFORM 1 /* traceparent='00-33333333333333333333333333333333-4444444444444444-01',tracestate='congo=2' */;
            END;
            $$;",
        )?;

        harness.enable_tracing()?;

        // Persisting otel.traceparent or otel.tracestate via ALTER PROCEDURE must error
        for alter_sql in [
            "ALTER PROCEDURE proc_comment_override() SET otel.traceparent = '00-33333333333333333333333333333333-4444444444444444-01'",
            "ALTER PROCEDURE proc_comment_override() SET otel.tracestate = 'congo=2'",
        ] {
            verify_that!(harness.simple_query(alter_sql), err(anything()))?;
            let spans = harness.collect_spans_count(1, time::Duration::from_secs(1));
            verify_that!(
                spans,
                elements_are![matches_pattern!(Span {
                    name: eq("ALTER"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: some(eq(&SpanStatus::error(""))),
                    attributes: contains(eq(&KeyValue::new_string(
                        "db.response.status_code",
                        "22023"
                    ))),
                    ..
                })]
            )?;
        }

        // Mutating otel.traceparent or otel.tracestate inside a procedure must error
        // without leaking context to subsequent top-level queries
        for call_sql in [
            "CALL proc_set_traceparent() /* traceparent='00-11111111111111111111111111111111-2222222222222222-01',tracestate='rojo=1' */",
            "CALL proc_set_tracestate() /* traceparent='00-11111111111111111111111111111111-2222222222222222-01',tracestate='rojo=1' */",
        ] {
            verify_that!(harness.simple_query(call_sql), err(anything()))?;
            harness.simple_query("SELECT 1")?;

            let spans = harness.collect_spans_count(2, time::Duration::from_secs(1));
            verify_that!(
                spans,
                elements_are![
                    matches_pattern!(Span {
                        name: eq("CALL"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: some(eq(&SpanStatus::error(""))),
                        trace_id: eq(&vec![0x11u8; 16]),
                        parent_span_id: eq(&vec![0x22u8; 8]),
                        trace_state: eq("rojo=1"),
                        attributes: contains(eq(&KeyValue::new_string(
                            "db.response.status_code",
                            "22023"
                        ))),
                        ..
                    }),
                    matches_pattern!(Span {
                        name: eq("SELECT"),
                        kind: eq(&(SpanKind::Server as i32)),
                        status: is_empty(),
                        trace_id: not(eq(&vec![0x33u8; 16])),
                        parent_span_id: is_empty(),
                        trace_state: eq(""),
                        ..
                    }),
                ]
            )?;
        }

        // SQLCommenter headers on nested statements are ignored and keep the caller's context
        harness.simple_query("SET otel.trace_nested_statements = 10")?;
        harness.simple_query(
            "CALL proc_comment_override() /* traceparent='00-11111111111111111111111111111111-2222222222222222-01',tracestate='rojo=1' */",
        )?;

        let spans = harness.collect_spans_count(2, time::Duration::from_secs(1));
        let call_span = spans.iter().find(|s| s.name == "CALL").or_fail()?;
        verify_that!(
            spans,
            unordered_elements_are![
                matches_pattern!(Span {
                    name: eq("CALL"),
                    kind: eq(&(SpanKind::Server as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x11u8; 16]),
                    parent_span_id: eq(&vec![0x22u8; 8]),
                    trace_state: eq("rojo=1"),
                    ..
                }),
                matches_pattern!(Span {
                    name: eq("SELECT"),
                    kind: eq(&(SpanKind::Internal as i32)),
                    status: is_empty(),
                    trace_id: eq(&vec![0x11u8; 16]),
                    parent_span_id: eq(&call_span.span_id),
                    trace_state: eq("rojo=1"),
                    links: is_empty(),
                    ..
                }),
            ]
        )?;

        Ok(())
    }
}
