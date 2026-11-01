// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn parse_error_skips_post_parse_analyze() -> Result<()> {
        let filename = "parse_error_skips_post_parse_analyze";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        assert_that!(
            client.simple_query("SELECT * FROM non_existent_table"),
            err(anything())
        );

        // wait for backend error handling
        client.check_connection()?;

        // post_parse_analyze is skipped because error happens during transformation.
        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
                eq(&("emit_log_hook", "entry")), // ERROR: relation does not exist
                eq(&("xact_callback", "ABORT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn planning_error_aborts_without_executor() -> Result<()> {
        let filename = "planning_error_aborts_without_executor";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        assert_that!(client.simple_query("SELECT 1 / 0"), err(anything()));

        // wait for backend error handling
        client.check_connection()?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("emit_log_hook", "entry")), // ERROR: division by zero
                eq(&("xact_callback", "ABORT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn runtime_error_aborts_without_executor_finish() -> Result<()> {
        let filename = "runtime_error_aborts_without_executor_finish";
        let mut client = super::setup_test(filename);

        // Create table with data so division by zero occurs at execution time rather than planning time
        client.simple_query("CREATE TEMP TABLE t_err (value) AS VALUES (0)")?;

        // Clear trace file before running error query
        super::clear_trace(filename);
        assert_that!(
            client.simple_query("SELECT 1 / value FROM t_err"),
            err(anything())
        );

        // wait for backend error handling
        client.check_connection()?;

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
                eq(&("emit_log_hook", "entry")), // ERROR: division by zero
                eq(&("xact_callback", "ABORT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn syntax_error_bypasses_all_parse_and_plan_hooks() -> Result<()> {
        let filename = "syntax_error_bypasses_all_parse_and_plan_hooks";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        assert_that!(client.simple_query("blah"), err(anything()));

        // wait for backend error handling
        client.check_connection()?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // ERROR: syntax error at or near "blah"
                eq(&("xact_callback", "ABORT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn empty_query_skips_execution_pipeline() -> Result<()> {
        let filename = "empty_query_skips_execution_pipeline";
        let mut client = super::setup_test(filename);

        for sql in [
            "",
            "; ; ;",
            "/* traceparent='00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01' */",
        ] {
            scoped_trace!("{sql:?}");

            super::clear_trace(filename);
            let _ = client.simple_query(sql)?;

            verify_that!(
                super::read_trace(filename),
                elements_are![
                    eq(&("emit_log_hook", "entry")), // LOG: statement
                    eq(&("xact_callback", "PRE_COMMIT")),
                    eq(&("xact_callback", "COMMIT")),
                ]
            )?;
        }

        Ok(())
    }

    #[pgrx::pg_test]
    fn simple_query_batch_planning_error_on_second_statement() -> Result<()> {
        let filename = "simple_query_batch_planning_error_on_second_statement";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        assert_that!(
            client.simple_query("SELECT 1; SELECT 1 / 0; SELECT 2"),
            err(anything())
        );

        client.check_connection()?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
                // Statement 1: SELECT 1 executes fully and commits
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
                // Statement 2: SELECT 1 / 0 enters planner_hook then aborts in constant folding
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("emit_log_hook", "entry")), // ERROR: division by zero
                eq(&("xact_callback", "ABORT")),
                // Statement 3: never executed
            ]
        )
    }

    #[pgrx::pg_test]
    fn simple_query_batch_syntax_error_aborts_entire_batch() -> Result<()> {
        let filename = "simple_query_batch_syntax_error_aborts_entire_batch";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        assert_that!(
            client.simple_query("SELECT 1; blah; SELECT 2"),
            err(anything())
        );

        client.check_connection()?;

        // raw_parser parses the entire batch upfront; syntax error aborts before statement 1 starts
        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // ERROR: syntax error at or near "blah"
                eq(&("xact_callback", "ABORT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn simple_query_batch_parse_error_on_second_statement() -> Result<()> {
        let filename = "simple_query_batch_parse_error_on_second_statement";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        assert_that!(
            client.simple_query("SELECT 1; SELECT * FROM non_existent_table; SELECT 2"),
            err(anything())
        );

        client.check_connection()?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
                // Statement 1 executes fully and commits
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
                // Statement 2: fails during parse analysis (transformStmt), skipping post_parse_analyze & planner
                eq(&("emit_log_hook", "entry")), // ERROR: relation does not exist
                eq(&("xact_callback", "ABORT")),
                // Statement 3 never executed
            ]
        )
    }

    #[pgrx::pg_test]
    fn simple_query_batch_runtime_error_on_second_statement() -> Result<()> {
        let filename = "simple_query_batch_runtime_error_on_second_statement";
        let mut client = super::setup_test(filename);

        client.simple_query("CREATE TEMP TABLE t_err_batch (value) AS VALUES (0)")?;
        super::clear_trace(filename);

        assert_that!(
            client.simple_query("SELECT 1; SELECT 1 / value FROM t_err_batch; SELECT 2"),
            err(anything())
        );

        client.check_connection()?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
                // Statement 1 executes fully and commits
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
                // Statement 2: plans, starts execution, and fails in ExecutorRun
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("planner_hook", "end")),
                eq(&("ExecutorStart_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "end")),
                eq(&("ExecutorStart_hook", "end")),
                eq(&("ExecutorRun_hook", "start")),
                eq(&("emit_log_hook", "entry")), // ERROR: division by zero
                eq(&("xact_callback", "ABORT")),
                // Statement 3 never executed
            ]
        )
    }
}
