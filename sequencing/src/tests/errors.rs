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
}
