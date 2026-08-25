// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use googletest::prelude::*;
    use std::io::Write;

    #[pgrx::pg_test]
    fn client_authentication_hook_records_connection_status() -> Result<()> {
        let filename = "client_authentication_hook_records_connection_status";
        let _client = super::setup_test(filename);

        // ClientAuthentication_hook fires during initial connection setup before session GUC is loaded, writing to default.tsv
        verify_that!(
            super::read_trace("default"),
            contains(eq(&("ClientAuthentication_hook", "STATUS_OK")))
        )
    }

    #[pgrx::pg_test]
    fn ddl_statement_bypasses_executor() -> Result<()> {
        let filename = "ddl_statement_bypasses_executor";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("CREATE TEMP TABLE t_seq_ddl (id int)")?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn create_view_invokes_utility_and_parse_hook() -> Result<()> {
        let filename = "create_view_invokes_utility_and_parse_hook";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("CREATE TEMP VIEW v_seq AS SELECT 1")?;

        // View: inner post_parse_analyze for definition query; no planner; no executor
        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
                // CREATE VIEW
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                // SELECT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    // NOTE: REFRESH MATERIALIZED VIEW and CREATE TABLE AS are very similar; utility + parser + executor.
    #[pgrx::pg_test]
    fn create_materialized_view_utility_with_executor_pipeline() -> Result<()> {
        let filename = "create_materialized_view_utility_with_executor_pipeline";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("CREATE MATERIALIZED VIEW mv_seq AS SELECT 1")?;

        let mut expected = vec![
            ("emit_log_hook", "entry"), // LOG: statement
            // CREATE
            ("post_parse_analyze_hook", "start"),
            ("post_parse_analyze_hook", "end"),
            ("ProcessUtility_hook", "start"),
        ];

        // PG18+ parses the view definition subquery inside ProcessUtility_hook
        if cfg!(any(feature = "pg18", feature = "pg19")) {
            // SELECT
            expected.push(("post_parse_analyze_hook", "start"));
            expected.push(("post_parse_analyze_hook", "end"));
        }

        // planner + executor
        expected.extend([
            ("planner_hook", "start"),
            ("planner_hook", "end"),
            ("ExecutorStart_hook", "start"),
            ("ExecutorCheckPerms_hook", "start"),
            ("ExecutorCheckPerms_hook", "end"),
            ("ExecutorStart_hook", "end"),
            ("ExecutorRun_hook", "start"),
        ]);

        // PG13/14 performs a secondary permission check during ExecutorRun
        if cfg!(any(feature = "pg13", feature = "pg14")) {
            expected.push(("ExecutorCheckPerms_hook", "start"));
            expected.push(("ExecutorCheckPerms_hook", "end"));
        }

        expected.extend([
            ("ExecutorRun_hook", "end"),
            ("ExecutorFinish_hook", "start"),
            ("ExecutorFinish_hook", "end"),
            ("ExecutorEnd_hook", "start"),
            ("ExecutorEnd_hook", "end"),
            ("ProcessUtility_hook", "end"),
            ("xact_callback", "PRE_COMMIT"),
            ("xact_callback", "COMMIT"),
        ]);

        verify_that!(super::read_trace(filename), container_eq(expected))
    }

    #[pgrx::pg_test]
    fn copy_from_stdin_bypasses_executor() -> Result<()> {
        let filename = "copy_from_stdin_bypasses_executor";
        let mut client = super::setup_test(filename);

        client.simple_query("CREATE TEMP TABLE t_cp (id int);")?;
        super::clear_trace(filename);

        let mut writer = client.copy_in("COPY t_cp FROM STDIN")?;
        writer.write_all(b"1\n2\n3\n")?;
        writer.finish()?;

        // Copy: no planner; no executor run
        verify_that!(
            super::read_trace(filename),
            elements_are![
                // Parse ('P') message
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
                // Execute ('E') message
                eq(&("emit_log_hook", "entry")), // LOG: statement: COPY
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "end")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn do_block_executes_spi_inside_utility() -> Result<()> {
        let filename = "do_block_executes_spi_inside_utility";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("DO $$ BEGIN PERFORM 1; END $$")?;

        // Do block: ProcessUtility for outer, SPI for inner
        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement
                // DO
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
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
                //
                eq(&("ProcessUtility_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }
}
