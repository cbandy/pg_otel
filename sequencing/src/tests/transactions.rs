// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn explicit_transaction_runs_xact_callback_at_commit() -> Result<()> {
        let filename = "explicit_transaction_runs_xact_callback_at_commit";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("BEGIN; SELECT 42; COMMIT")?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement; BEGIN + SELECT + COMMIT
                // BEGIN
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
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
                eq(&("ExecutorRun_hook", "end")),
                eq(&("ExecutorFinish_hook", "start")),
                eq(&("ExecutorFinish_hook", "end")),
                eq(&("ExecutorEnd_hook", "start")),
                eq(&("ExecutorEnd_hook", "end")),
                // COMMIT
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
    fn explicit_rollback_runs_xact_callback_with_abort() -> Result<()> {
        let filename = "explicit_rollback_runs_xact_callback_with_abort";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("BEGIN; SELECT 42; ROLLBACK")?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement; BEGIN + SELECT + ROLLBACK
                // BEGIN
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
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
                eq(&("ExecutorRun_hook", "end")),
                eq(&("ExecutorFinish_hook", "start")),
                eq(&("ExecutorFinish_hook", "end")),
                eq(&("ExecutorEnd_hook", "start")),
                eq(&("ExecutorEnd_hook", "end")),
                // ROLLBACK
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("xact_callback", "ABORT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn extended_query_transaction_error_ends_transaction() -> Result<()> {
        let filename = "extended_query_transaction_error_ends_transaction";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.execute("BEGIN", &[])?;
        assert_that!(client.execute("SELECT 1 / 0", &[]), err(anything()));
        assert_that!(
            client.execute("SELECT 1", &[]),
            err(predicate(|e: &postgres::Error| {
                e.code() == Some(&postgres::error::SqlState::IN_FAILED_SQL_TRANSACTION)
            }))
        );
        client.execute("ROLLBACK", &[])?;
        client.execute("SELECT 1", &[])?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                // BEGIN
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
                eq(&("emit_log_hook", "entry")), // LOG: statement: BEGIN
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                // SELECT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("emit_log_hook", "entry")), // ERROR: division by zero
                eq(&("xact_callback", "ABORT")),
                // SELECT; fails
                eq(&("emit_log_hook", "entry")), // LOG: statement: SELECT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("emit_log_hook", "entry")), // ERROR: current transaction is aborted
                // ROLLBACK
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                // SELECT; succeeds (after ROLLBACK)
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
                eq(&("planner_hook", "start")),
                eq(&("planner_hook", "end")),
                eq(&("ExecutorStart_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "end")),
                eq(&("ExecutorStart_hook", "end")),
                eq(&("emit_log_hook", "entry")), // LOG: statement: SELECT
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
    fn simple_query_transaction_error_exits_early() -> Result<()> {
        let filename = "simple_query_transaction_error_exits_early";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        // Postgres ignores any statements that follow an error during Simple Query ('Q')
        assert_that!(
            client.simple_query("BEGIN; SELECT 1/0; ROLLBACK"), // ROLLBACK does *not* run here
            err(anything())
        );
        assert_that!(
            client.execute("SELECT 1", &[]),
            err(predicate(|e: &postgres::Error| {
                e.code() == Some(&postgres::error::SqlState::IN_FAILED_SQL_TRANSACTION)
            }))
        );
        client.execute("ROLLBACK", &[])?;
        client.execute("SELECT 1", &[])?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement; BEGIN + SELECT 1/0 + ROLLBACK
                // BEGIN
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                // SELECT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("emit_log_hook", "entry")), // ERROR: division by zero
                eq(&("xact_callback", "ABORT")),
                // SELECT; fails
                eq(&("emit_log_hook", "entry")), // LOG: statement: SELECT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("emit_log_hook", "entry")), // ERROR: current transaction is aborted
                // ROLLBACK
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                // SELECT; succeeds (after ROLLBACK)
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
                eq(&("planner_hook", "start")),
                eq(&("planner_hook", "end")),
                eq(&("ExecutorStart_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "start")),
                eq(&("ExecutorCheckPerms_hook", "end")),
                eq(&("ExecutorStart_hook", "end")),
                eq(&("emit_log_hook", "entry")), // LOG: statement: SELECT
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
    fn extended_query_savepoint_rollback_recovers_transaction() -> Result<()> {
        let filename = "extended_query_savepoint_rollback_recovers_transaction";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.execute("BEGIN", &[])?;
        client.execute("SAVEPOINT s1", &[])?;
        assert_that!(client.execute("SELECT 1 / 0", &[]), err(anything()));
        assert_that!(
            client.execute("SELECT 1", &[]),
            err(predicate(|e: &postgres::Error| {
                e.code() == Some(&postgres::error::SqlState::IN_FAILED_SQL_TRANSACTION)
            }))
        );
        client.execute("ROLLBACK TO s1", &[])?;
        client.execute("COMMIT", &[])?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                // BEGIN
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
                eq(&("emit_log_hook", "entry")), // LOG: statement: BEGIN
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                // SAVEPOINT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("emit_log_hook", "entry")), // LOG: statement: SAVEPOINT
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("subxact_callback", "START_SUB")),
                // SELECT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("emit_log_hook", "entry")), // ERROR: division by zero
                eq(&("subxact_callback", "ABORT_SUB")),
                // SELECT; fails
                eq(&("emit_log_hook", "entry")), // LOG: statement: SELECT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("emit_log_hook", "entry")), // ERROR: current transaction is aborted
                // ROLLBACK
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("subxact_callback", "START_SUB")),
                // COMMIT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("emit_log_hook", "entry")), // LOG: statement: COMMIT
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("subxact_callback", "OTHER_SUB")),
                eq(&("subxact_callback", "COMMIT_SUB")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn simple_query_savepoint_error_exits_early() -> Result<()> {
        let filename = "simple_query_savepoint_error_exits_early";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        // Postgres ignores any statements that follow an error during Simple Query ('Q')
        assert_that!(
            client.simple_query("BEGIN; SAVEPOINT s1; SELECT 1 / 0; ROLLBACK TO s1; COMMIT"), // ROLLBACK does *not* run here
            err(anything())
        );
        assert_that!(
            client.execute("SELECT 1", &[]),
            err(predicate(|e: &postgres::Error| {
                e.code() == Some(&postgres::error::SqlState::IN_FAILED_SQL_TRANSACTION)
            }))
        );
        client.execute("ROLLBACK TO s1", &[])?;
        client.execute("COMMIT", &[])?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement; BEGIN + SAVEPOINT + SELECT + ROLLBACK + COMMIT
                // BEGIN
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                // SAVEPOINT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("subxact_callback", "START_SUB")),
                // SELECT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("planner_hook", "start")),
                eq(&("emit_log_hook", "entry")), // ERROR: division by zero
                eq(&("subxact_callback", "ABORT_SUB")),
                // SELECT; fails
                eq(&("emit_log_hook", "entry")), // LOG: statement: SELECT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("emit_log_hook", "entry")), // ERROR: current transaction is aborted
                // ROLLBACK
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("subxact_callback", "START_SUB")),
                // COMMIT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("emit_log_hook", "entry")), // LOG: statement: COMMIT
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("subxact_callback", "OTHER_SUB")),
                eq(&("subxact_callback", "COMMIT_SUB")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    #[pgrx::pg_test]
    fn rollback_over_savepoints() -> Result<()> {
        let filename = "rollback_over_savepoint";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("BEGIN; SAVEPOINT s1; SAVEPOINT s2; ROLLBACK")?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement; BEGIN + SAVEPOINT + SAVEPOINT + ROLLBACK
                // BEGIN
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                // SAVEPOINT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("subxact_callback", "START_SUB")),
                // SAVEPOINT
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("subxact_callback", "START_SUB")),
                // ROLLBACK
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("subxact_callback", "ABORT_SUB")), // s2
                eq(&("subxact_callback", "ABORT_SUB")), // s1
                eq(&("xact_callback", "ABORT")),
            ]
        )
    }
}
