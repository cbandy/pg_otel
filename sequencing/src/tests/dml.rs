// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn simple_select_is_one_pipeline() -> Result<()> {
        let filename = "simple_select_is_one_pipeline";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1")?;

        // Standard SELECT execution flow
        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
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
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn materialized_cte_barrier_is_one_pipeline() -> Result<()> {
        let filename = "materialized_cte_barrier_is_one_pipeline";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("WITH c AS MATERIALIZED (SELECT 1) SELECT * FROM c")?;

        // Materialized CTE: single planner and executor invocation
        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
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
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn recursive_cte_is_one_pipeline() -> Result<()> {
        let filename = "recursive_cte_is_one_pipeline";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("WITH RECURSIVE c(n) AS (VALUES (1) UNION ALL SELECT n+1 FROM c WHERE n < 3) SELECT * FROM c")?;

        // Recursive CTE: single executor
        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
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
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    // NOTE: MERGE looks very similar: parser + planner + executor.
    #[pgrx::pg_test]
    fn upsert_is_one_pipeline() -> Result<()> {
        let filename = "upsert_is_one_pipeline";
        let mut client = super::setup_test(filename);

        client.simple_query("CREATE TEMP TABLE t_up (id int primary key, value text);")?;
        client.simple_query("INSERT INTO t_up VALUES (1, 'a');")?;
        super::clear_trace(filename);

        client.execute(
            "INSERT INTO t_up VALUES (1, 'b') ON CONFLICT (id) DO UPDATE SET value = EXCLUDED.value",
            &[],
        )?;

        // Speculative insert then update is one execution
        verify_that!(
            super::read_trace(filename),
            elements_are![
                // Parse ('P') message
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
                // Execute ('E') message
                eq(&("planner_hook", "start")),
                eq(&("planner_hook", "end")),
                eq(&("emit_log_hook", "entry")), // LOG: statement
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
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn parallel_query_executes_parallel_commit_callback() -> Result<()> {
        let filename = "parallel_query_executes_parallel_commit_callback";
        let mut client = super::setup_test(filename);

        client.simple_query(cfg_select! {
            any(feature = "pg13", feature = "pg14", feature = "pg15") => {
                "SET force_parallel_mode = regress; SET max_parallel_workers_per_gather = 1"
            }
            _ => {
                "SET debug_parallel_query = regress; SET max_parallel_workers_per_gather = 1"
            }
        })?;

        client.simple_query("CREATE TABLE t_par AS SELECT generate_series(1, 1000) AS id")?;
        super::clear_trace(filename);

        client.simple_query("SELECT count(*) FROM t_par")?;

        let trace = super::read_trace(filename);
        client.simple_query("DROP TABLE t_par")?;

        let mut expected = Vec::new();

        // Postgres (leader) makes a parallel plan
        expected.extend([
            ("emit_log_hook", "entry"), // LOG: statement
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("planner_hook", "start"),
            ("planner_hook", "end"),
            ("ExecutorStart_hook", "start"),
            ("ExecutorCheckPerms_hook", "start"),
            ("ExecutorCheckPerms_hook", "end"),
            ("ExecutorStart_hook", "end"),
            ("ExecutorRun_hook", "start"),
        ]);

        // Parallel workers spawn and run to completion inside leader's ExecutorRun (fan out)
        // https://postgr.es/m/18545-feba138862f19aaa@postgresql.org
        #[cfg(any(
            feature = "pg13",
            feature = "pg14",
            feature = "pg15",
            feature = "pg16",
            feature = "pg17"
        ))]
        expected.extend([("xact_callback", "PRE_COMMIT"), ("xact_callback", "COMMIT")]);
        expected.extend([
            ("ExecutorStart_hook", "start"),
            ("ExecutorCheckPerms_hook", "start"),
            ("ExecutorCheckPerms_hook", "end"),
            ("ExecutorStart_hook", "end"),
            ("ExecutorRun_hook", "start"),
            ("ExecutorRun_hook", "end"),
            ("ExecutorFinish_hook", "start"),
            ("ExecutorFinish_hook", "end"),
            ("ExecutorEnd_hook", "start"),
            ("ExecutorEnd_hook", "end"),
            ("xact_callback", "PARALLEL_PRE_COMMIT"),
            ("xact_callback", "PARALLEL_COMMIT"),
        ]);

        // Postgres (leader) gathers results (fan in) and responds to client
        expected.extend([
            ("ExecutorRun_hook", "end"),
            ("ExecutorFinish_hook", "start"),
            ("ExecutorFinish_hook", "end"),
            ("ExecutorEnd_hook", "start"),
            ("ExecutorEnd_hook", "end"),
            ("xact_callback", "PRE_COMMIT"),
            ("xact_callback", "COMMIT"),
        ]);

        verify_that!(trace, container_eq(expected))
    }
}
