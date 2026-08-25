// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn extended_query_cursor_fetch_executes_incrementally() -> Result<()> {
        let filename = "extended_query_cursor_fetch_executes_incrementally";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.execute("BEGIN", &[])?;
        client.execute("DECLARE c CURSOR FOR SELECT 1", &[])?;
        client.execute("FETCH 1 FROM c", &[])?;
        client.execute("CLOSE c", &[])?;
        client.execute("COMMIT", &[])?;

        let mut expected = vec![
            // BEGIN
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("xact_callback", "PRE_COMMIT"),
            ("xact_callback", "COMMIT"),
            ("emit_log_hook", "entry"), // LOG: statement: BEGIN
            ("ProcessUtility_hook", "start"),
            ("ProcessUtility_hook", "end"),
            // DECLARE
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("emit_log_hook", "entry"), // LOG: statement: DECLARE
            ("ProcessUtility_hook", "start"),
        ];

        // PG18+ parses the cursor target subquery inside ProcessUtility_hook
        if cfg!(any(feature = "pg18", feature = "pg19")) {
            expected.push(("post_parse_analyze_hook", "start"));
            expected.push(("post_parse_analyze_hook", "end"));
        }

        expected.extend([
            ("planner_hook", "start"),
            ("planner_hook", "end"),
            ("ExecutorStart_hook", "start"),
            ("ExecutorCheckPerms_hook", "start"),
            ("ExecutorCheckPerms_hook", "end"),
            ("ExecutorStart_hook", "end"),
            ("ProcessUtility_hook", "end"),
            // FETCH
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("emit_log_hook", "entry"), // LOG: statement: FETCH
            ("ProcessUtility_hook", "start"),
            ("ExecutorRun_hook", "start"),
            ("ExecutorRun_hook", "end"),
            ("ProcessUtility_hook", "end"),
            // CLOSE
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("emit_log_hook", "entry"), // LOG: statement: CLOSE
            ("ProcessUtility_hook", "start"),
            ("ExecutorFinish_hook", "start"),
            ("ExecutorFinish_hook", "end"),
            ("ExecutorEnd_hook", "start"),
            ("ExecutorEnd_hook", "end"),
            ("ProcessUtility_hook", "end"),
            // COMMIT
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("emit_log_hook", "entry"), // LOG: statement: COMMIT
            ("ProcessUtility_hook", "start"),
            ("ProcessUtility_hook", "end"),
            ("xact_callback", "PRE_COMMIT"),
            ("xact_callback", "COMMIT"),
        ]);

        verify_that!(super::read_trace(filename), container_eq(expected))
    }

    #[pgrx::pg_test]
    fn simple_query_cursor_fetch_executes_incrementally() -> Result<()> {
        let filename = "simple_query_cursor_fetch_executes_incrementally";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query(
            "BEGIN; DECLARE c CURSOR FOR SELECT 1; FETCH 1 FROM c; CLOSE c; COMMIT",
        )?;

        let mut expected = vec![
            ("emit_log_hook", "entry"), // LOG: statement: BEGIN + DECLARE + FETCH + CLOSE + COMMIT
            // BEGIN
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("ProcessUtility_hook", "start"),
            ("ProcessUtility_hook", "end"),
            // DECLARE
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("ProcessUtility_hook", "start"),
        ];

        // PG18+ parses the cursor target subquery inside ProcessUtility_hook
        if cfg!(any(feature = "pg18", feature = "pg19")) {
            expected.push(("post_parse_analyze_hook", "start"));
            expected.push(("post_parse_analyze_hook", "end"));
        }

        expected.extend([
            ("planner_hook", "start"),
            ("planner_hook", "end"),
            ("ExecutorStart_hook", "start"),
            ("ExecutorCheckPerms_hook", "start"),
            ("ExecutorCheckPerms_hook", "end"),
            ("ExecutorStart_hook", "end"),
            ("ProcessUtility_hook", "end"),
            // FETCH
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("ProcessUtility_hook", "start"),
            ("ExecutorRun_hook", "start"),
            ("ExecutorRun_hook", "end"),
            ("ProcessUtility_hook", "end"),
            // CLOSE
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("ProcessUtility_hook", "start"),
            ("ExecutorFinish_hook", "start"),
            ("ExecutorFinish_hook", "end"),
            ("ExecutorEnd_hook", "start"),
            ("ExecutorEnd_hook", "end"),
            ("ProcessUtility_hook", "end"),
            // COMMIT
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("ProcessUtility_hook", "start"),
            ("ProcessUtility_hook", "end"),
            ("xact_callback", "PRE_COMMIT"),
            ("xact_callback", "COMMIT"),
        ]);

        verify_that!(super::read_trace(filename), container_eq(expected))
    }
}
