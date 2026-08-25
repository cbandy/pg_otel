// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn explain_select_skips_executor_run_finish() -> Result<()> {
        let filename = "explain_select_skips_executor_run_finish";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("EXPLAIN SELECT 1")?;

        let mut expected = vec![
            ("emit_log_hook", "entry"), // LOG: statement
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("ProcessUtility_hook", "start"),
        ];

        // PG15+ parses the target query inside ProcessUtility_hook
        if cfg!(not(any(feature = "pg13", feature = "pg14"))) {
            expected.push(("post_parse_analyze_hook", "start"));
            expected.push(("post_parse_analyze_hook", "end"));
        }

        // Every PG calls ExplainOneQuery_hook here, but PG17 makes it much easier to implement.
        if cfg!(any(feature = "pg17", feature = "pg18", feature = "pg19")) {
            expected.push(("ExplainOneQuery_hook", "start"));
        }

        // ExecutorRun is skipped because this EXPLAIN lacks ANALYZE.
        expected.extend([
            ("planner_hook", "start"),
            ("planner_hook", "end"),
            ("ExecutorStart_hook", "start"),
            ("ExecutorCheckPerms_hook", "start"),
            ("ExecutorCheckPerms_hook", "end"),
            ("ExecutorStart_hook", "end"),
            ("ExecutorEnd_hook", "start"),
            ("ExecutorEnd_hook", "end"),
        ]);

        // Every PG has ExplainOneQuery_hook here, but PG17 makes it much easier to implement.
        if cfg!(any(feature = "pg17", feature = "pg18", feature = "pg19")) {
            expected.push(("ExplainOneQuery_hook", "end"));
        }

        expected.extend([
            ("ProcessUtility_hook", "end"),
            ("xact_callback", "PRE_COMMIT"),
            ("xact_callback", "COMMIT"),
        ]);

        verify_that!(super::read_trace(filename), container_eq(expected))
    }

    #[pgrx::pg_test]
    fn explain_analyze_select_invokes_executor() -> Result<()> {
        let filename = "explain_analyze_select_invokes_executor";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("EXPLAIN (ANALYZE) SELECT 1")?;

        let mut expected = vec![
            ("emit_log_hook", "entry"), // LOG: statement
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("ProcessUtility_hook", "start"),
        ];

        // PG15+ parses the target query inside ProcessUtility_hook
        if cfg!(not(any(feature = "pg13", feature = "pg14"))) {
            expected.push(("post_parse_analyze_hook", "start"));
            expected.push(("post_parse_analyze_hook", "end"));
        }

        // Every PG calls ExplainOneQuery_hook here, but PG17 makes it much easier to implement.
        if cfg!(any(feature = "pg17", feature = "pg18", feature = "pg19")) {
            expected.push(("ExplainOneQuery_hook", "start"));
        }

        expected.extend([
            ("planner_hook", "start"),
            ("planner_hook", "end"),
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
        ]);

        // Every PG has ExplainOneQuery_hook here, but PG17 makes it much easier to implement.
        if cfg!(any(feature = "pg17", feature = "pg18", feature = "pg19")) {
            expected.push(("ExplainOneQuery_hook", "end"));
        }

        expected.extend([
            ("ProcessUtility_hook", "end"),
            ("xact_callback", "PRE_COMMIT"),
            ("xact_callback", "COMMIT"),
        ]);

        verify_that!(super::read_trace(filename), container_eq(expected))
    }
}
