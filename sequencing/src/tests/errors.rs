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
}
