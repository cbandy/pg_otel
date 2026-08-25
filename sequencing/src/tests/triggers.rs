// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn plpgsql_function_is_nested_pipelines() -> Result<()> {
        let filename = "plpgsql_function_is_nested_pipelines";
        let mut client = super::setup_test(filename);

        client.simple_query(
            "CREATE FUNCTION fn_seq() RETURNS int LANGUAGE plpgsql AS $$ \
             BEGIN \
                 PERFORM 1; \
                 RETURN 42; \
             END; $$;",
        )?;
        super::clear_trace(filename);

        client.simple_query("SELECT fn_seq()")?;

        // PL/pgSQL: outer ExecutorRun invokes inner pipeline (parser + executor)
        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement: SELECT
                // SELECT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("planner_hook", "end")),
                eq(&("ExecutorStart_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "end")),
                eq(&("ExecutorStart_hook", "end")),
                eq(&("ExecutorRun_hook", "start")),
                // PERFORM
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("planner_hook", "end")),
                eq(&("ExecutorStart_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "end")),
                eq(&("ExecutorStart_hook", "end")),
                eq(&("ExecutorRun_hook", "start")),
                eq(&("ExecutorRun_hook", "end")),
                eq(&("ExecutorFinish_hook", "start")),
                eq(&("ExecutorFinish_hook", "end")),
                eq(&("ExecutorEnd_hook", "start")),
                eq(&("ExecutorEnd_hook", "end")),
                // RETURN
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("planner_hook", "end")),
                //
                eq(&("ExecutorRun_hook", "end")),
                eq(&("ExecutorFinish_hook", "start")),
                eq(&("ExecutorFinish_hook", "end")),
                eq(&("ExecutorEnd_hook", "start")),
                eq(&("ExecutorEnd_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    // NOTE: RLS looks very similar to this when the policy calls a function.
    #[pgrx::pg_test]
    fn triggers_are_nested_pipelines() -> Result<()> {
        let filename = "triggers_are_nested_pipelines";
        let mut client = super::setup_test(filename);

        client.simple_query(
            "CREATE TEMP TABLE t_dst (id int); \
             CREATE TEMP TABLE t_src (id int); \
             CREATE FUNCTION fn_trig() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN \
                 INSERT INTO t_dst VALUES (NEW.id); \
                 RETURN NEW; \
             END; $$; \
             CREATE TRIGGER tr_seq BEFORE INSERT ON t_src \
             FOR EACH ROW EXECUTE FUNCTION fn_trig();",
        )?;

        // Force immediate plan caching, bypassing PostgreSQL's default "auto" 5-execution threshold
        client.simple_query("SET plan_cache_mode = force_generic_plan")?;
        super::clear_trace(filename);

        client.simple_query("INSERT INTO t_src VALUES (10), (20)")?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement: INSERT
                // INSERT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("planner_hook", "end")),
                eq(&("ExecutorStart_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "end")),
                eq(&("ExecutorStart_hook", "end")),
                eq(&("ExecutorRun_hook", "start")),
                // Trigger; first run includes planner
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("planner_hook", "end")),
                eq(&("ExecutorStart_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "end")),
                eq(&("ExecutorStart_hook", "end")),
                eq(&("ExecutorRun_hook", "start")),
                eq(&("ExecutorRun_hook", "end")),
                eq(&("ExecutorFinish_hook", "start")),
                eq(&("ExecutorFinish_hook", "end")),
                eq(&("ExecutorEnd_hook", "start")),
                eq(&("ExecutorEnd_hook", "end")),
                // Trigger; subsequent runs exclude planner (after generic plan)
                eq(&("ExecutorStart_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "end")),
                eq(&("ExecutorStart_hook", "end")),
                eq(&("ExecutorRun_hook", "start")),
                eq(&("ExecutorRun_hook", "end")),
                eq(&("ExecutorFinish_hook", "start")),
                eq(&("ExecutorFinish_hook", "end")),
                eq(&("ExecutorEnd_hook", "start")),
                eq(&("ExecutorEnd_hook", "end")),
                //
                eq(&("ExecutorRun_hook", "end")),
                eq(&("ExecutorFinish_hook", "start")),
                eq(&("ExecutorFinish_hook", "end")),
                eq(&("ExecutorEnd_hook", "start")),
                eq(&("ExecutorEnd_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn event_trigger_invokes_nested_pipeline_inside_process_utility() -> Result<()> {
        let filename = "event_trigger_invokes_nested_pipeline_inside_process_utility";
        let mut client = super::setup_test(filename);

        client.simple_query(
            "CREATE OR REPLACE FUNCTION fn_evt_trig() RETURNS event_trigger LANGUAGE plpgsql AS $$ \
             BEGIN \
                 PERFORM 1; \
             END; $$; \
             CREATE EVENT TRIGGER tr_evt ON ddl_command_start EXECUTE FUNCTION fn_evt_trig();",
        )?;
        super::clear_trace(filename);

        client.simple_query("CREATE TEMP TABLE t_evt (id int)")?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement: CREATE
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                // Trigger; PERFORM
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("planner_hook", "end")),
                eq(&("ExecutorStart_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "end")),
                eq(&("ExecutorStart_hook", "end")),
                eq(&("ExecutorRun_hook", "start")),
                eq(&("ExecutorRun_hook", "end")),
                eq(&("ExecutorFinish_hook", "start")),
                eq(&("ExecutorFinish_hook", "end")),
                eq(&("ExecutorEnd_hook", "start")),
                eq(&("ExecutorEnd_hook", "end")),
                //
                eq(&("ProcessUtility_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )?;

        client.simple_query("DROP EVENT TRIGGER tr_evt; DROP FUNCTION fn_evt_trig()")?;
        Ok(())
    }
}
