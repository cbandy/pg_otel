// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use googletest::prelude::*;

    /// Filters trace log lines to events that have SQL contents.
    fn pipeline_starts(filename: &str) -> Vec<&'static str> {
        super::read_trace_lines(filename)
            .into_iter()
            .filter(|line| {
                line.starts_with("post_parse_analyze_hook\tstart")
                    || line.starts_with("planner_hook\tstart")
                    || line.starts_with("ExecutorStart_hook\tstart")
                    || line.starts_with("ProcessUtility_hook\tstart")
                    || line.starts_with("ExplainOneQuery_hook\tstart")
            })
            .collect()
    }

    /// Verifies baseline offsets for a single query with semicolon.
    ///
    /// The statement begins at `loc=0` and covers `SELECT 1` (`len=8`), excluding the terminating
    /// semicolon. All hooks have identical offsets.
    #[pgrx::pg_test]
    fn full_pipeline_offsets() -> Result<()> {
        let filename = "full_pipeline_offsets";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1;")?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(&"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1;\""),
                eq(&"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1;\""),
                eq(&"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1;\""),
            ]
        )
    }

    /// Verifies offset behavior when a statement is preceded by a SQL comment.
    ///
    /// - v18+ starts at `SELECT` (`loc=11`) with `len=8` up to (excluding) the semicolon;
    ///   the leading comment sits outside the statement slice.
    /// - v13–17 starts the first statement at `loc=0`; `len=19` includes the leading comment
    ///   and excludes the terminating semicolon.
    #[pgrx::pg_test]
    fn leading_comment() -> Result<()> {
        let filename = "leading_comment";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("/* tp=1 */ SELECT 1;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=19 sql=\"/* tp=1 */ SELECT 1;\""),
                        eq(&"planner_hook\tstart\tloc=0 len=19 sql=\"/* tp=1 */ SELECT 1;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=19 sql=\"/* tp=1 */ SELECT 1;\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=11 len=8 sql=\"/* tp=1 */ SELECT 1;\""),
                        eq(&"planner_hook\tstart\tloc=11 len=8 sql=\"/* tp=1 */ SELECT 1;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=11 len=8 sql=\"/* tp=1 */ SELECT 1;\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offset behavior for a comment inside the statement before the semicolon.
    ///
    /// Because the comment precedes the statement-terminating semicolon, all Postgres versions treat it
    /// as part of the statement body: `loc=0` and `len=19` spans the full text including the comment.
    ///
    /// NOTE: This is the standard SQLCommenter format: when input SQL includes a terminating semicolon,
    /// implementations strip the semicolon, append the `/* ... */` comment, and restore the semicolon
    /// to ensure the comment remains part of the statement.
    #[pgrx::pg_test]
    fn trailing_comment_pre_semicolon() -> Result<()> {
        let filename = "trailing_comment_pre_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1 /* tp=1 */;")?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(&"post_parse_analyze_hook\tstart\tloc=0 len=19 sql=\"SELECT 1 /* tp=1 */;\""),
                eq(&"planner_hook\tstart\tloc=0 len=19 sql=\"SELECT 1 /* tp=1 */;\""),
                eq(&"ExecutorStart_hook\tstart\tloc=0 len=19 sql=\"SELECT 1 /* tp=1 */;\""),
            ]
        )
    }

    /// Verifies offset behavior for a comment after the statement-terminating semicolon.
    ///
    /// The statement is terminated at the semicolon at byte 8 (`loc=0 len=8`). The trailing comment
    /// is outside the statement boundary and is excluded from execution pipeline offsets.
    #[pgrx::pg_test]
    fn trailing_comment_post_semicolon() -> Result<()> {
        let filename = "trailing_comment_post_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1; /* tp=1 */")?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(&"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* tp=1 */\""),
                eq(&"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* tp=1 */\""),
                eq(&"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* tp=1 */\""),
            ]
        )
    }

    /// Verifies offsets for a multi-query batch.
    ///
    /// - Statement 1 is identical in all versions: `loc=0 len=8` covers `SELECT 1`.
    /// - Statement 2:
    ///   - v18+ starts at `SELECT`, so `loc=10 len=8` cleanly covers `SELECT 2`.
    ///   - v13–17 starts just after the semicolon, so `loc=9 len=9` starts at the space character.
    #[pgrx::pg_test]
    fn multi_statement_batch() -> Result<()> {
        let filename = "multi_statement_batch";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1; SELECT 2;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2;\""),
                        eq(&"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2;\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=9 len=9 sql=\"SELECT 1; SELECT 2;\""),
                        eq(&"planner_hook\tstart\tloc=9 len=9 sql=\"SELECT 1; SELECT 2;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=9 len=9 sql=\"SELECT 1; SELECT 2;\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2;\""),
                        eq(&"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2;\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=10 len=8 sql=\"SELECT 1; SELECT 2;\""),
                        eq(&"planner_hook\tstart\tloc=10 len=8 sql=\"SELECT 1; SELECT 2;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=10 len=8 sql=\"SELECT 1; SELECT 2;\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets for a batch with an interstitial comment.
    ///
    /// - Statement 1 is identical in all versions: `loc=0 len=8` covers `SELECT 1`.
    /// - Statement 2:
    ///   - v18+ starts at `SELECT`, so `loc=21 len=8` cleanly isolates `SELECT 2`; the comment is outside the slice.
    ///   - v13–17 starts just after the semicolon, so `loc=9 len=20` includes the comment in the slice.
    #[pgrx::pg_test]
    fn interstitial_comment() -> Result<()> {
        let filename = "interstitial_comment";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1; /* tp=2 */ SELECT 2;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""
                        ),
                        eq(&"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=9 len=20 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""
                        ),
                        eq(&"planner_hook\tstart\tloc=9 len=20 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=9 len=20 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""
                        ),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""
                        ),
                        eq(&"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=21 len=8 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""
                        ),
                        eq(&"planner_hook\tstart\tloc=21 len=8 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=21 len=8 sql=\"SELECT 1; /* tp=2 */ SELECT 2;\""
                        ),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets for comments before and after statements in a query batch.
    ///
    /// - Statement 1 has a leading comment:
    ///   - v18+ starts at `SELECT` (`loc=11 len=8`) and excludes the leading comment.
    ///   - v13–17 the slice `loc=0 len=19` includes the leading comment.
    /// - Statement 2 has a trailing comment before its semicolon:
    ///   - v18+ starts at `SELECT` (`loc=21 len=19`) and includes the trailing comment.
    ///   - v13–17 starts just after the semicolon (`loc=20 len=20`) and includes the trailing comment.
    #[pgrx::pg_test]
    fn mixed_comments_in_batch() -> Result<()> {
        let filename = "mixed_comments_in_batch";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=19 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=0 len=19 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=19 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=20 len=20 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=20 len=20 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=20 len=20 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=11 len=8 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=11 len=8 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=11 len=8 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=21 len=19 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=21 len=19 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=21 len=19 sql=\"/* tp=1 */ SELECT 1; SELECT 2 /* tp=2 */;\""
                        ),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets across mixed utility and DML statements in a batch.
    ///
    /// - Statement 1 (DDL) spans the full statement (`loc=0 len=38`) excluding the semicolon in all versions.
    /// - Statement 2 (SELECT):
    ///   - v18+ targets `SELECT 1`, `loc=40 len=8`.
    ///   - v13–17 includes the leading space, `loc=39 len=9`.
    #[pgrx::pg_test]
    fn utility_statement_batch() -> Result<()> {
        let filename = "utility_statement_batch";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("CREATE TEMP TABLE t_util_test (id int); SELECT 1;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=38 sql=\"CREATE TEMP TABLE t_util_test (id int); SELECT 1;\""
                        ),
                        eq(
                            &"ProcessUtility_hook\tstart\tloc=0 len=38 sql=\"CREATE TEMP TABLE t_util_test (id int); SELECT 1;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=39 len=9 sql=\"CREATE TEMP TABLE t_util_test (id int); SELECT 1;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=39 len=9 sql=\"CREATE TEMP TABLE t_util_test (id int); SELECT 1;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=39 len=9 sql=\"CREATE TEMP TABLE t_util_test (id int); SELECT 1;\""
                        ),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=38 sql=\"CREATE TEMP TABLE t_util_test (id int); SELECT 1;\""
                        ),
                        eq(
                            &"ProcessUtility_hook\tstart\tloc=0 len=38 sql=\"CREATE TEMP TABLE t_util_test (id int); SELECT 1;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=40 len=8 sql=\"CREATE TEMP TABLE t_util_test (id int); SELECT 1;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=40 len=8 sql=\"CREATE TEMP TABLE t_util_test (id int); SELECT 1;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=40 len=8 sql=\"CREATE TEMP TABLE t_util_test (id int); SELECT 1;\""
                        ),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets for transaction control commands flanking a query.
    ///
    /// - `BEGIN`: Utility statement, `loc=0 len=5` in all versions.
    /// - `SELECT 1`: DML pipeline. v18+ covers `SELECT 1`, `loc=7 len=8`. v13–17 starts after semicolon, `loc=6 len=9`.
    /// - `COMMIT`: Utility statement. v18+ covers `COMMIT`, `loc=17 len=6`. v13–17 starts after semicolon, `loc=16 len=7`.
    #[pgrx::pg_test]
    fn transaction_commands_batch() -> Result<()> {
        let filename = "transaction_commands_batch";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("BEGIN; SELECT 1; COMMIT;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=5 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                        eq(&"ProcessUtility_hook\tstart\tloc=0 len=5 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=6 len=9 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                        eq(&"planner_hook\tstart\tloc=6 len=9 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=6 len=9 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=16 len=7 sql=\"BEGIN; SELECT 1; COMMIT;\""
                        ),
                        eq(&"ProcessUtility_hook\tstart\tloc=16 len=7 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=5 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                        eq(&"ProcessUtility_hook\tstart\tloc=0 len=5 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=7 len=8 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                        eq(&"planner_hook\tstart\tloc=7 len=8 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=7 len=8 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=17 len=6 sql=\"BEGIN; SELECT 1; COMMIT;\""
                        ),
                        eq(&"ProcessUtility_hook\tstart\tloc=17 len=6 sql=\"BEGIN; SELECT 1; COMMIT;\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets under the Extended Query Protocol.
    ///
    /// When executing parameterized queries via parse/bind/execute, Postgres does not delimit statements
    /// by semicolon; `loc=0 len=0` indicates that the entire query string constitutes the statement.
    #[pgrx::pg_test]
    fn extended_query_protocol() -> Result<()> {
        let filename = "extended_query_protocol";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.query("SELECT $1::int", &[&1i32])?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(&"post_parse_analyze_hook\tstart\tloc=0 len=0 sql=\"SELECT $1::int\""),
                eq(&"planner_hook\tstart\tloc=0 len=0 sql=\"SELECT $1::int\""),
                eq(&"ExecutorStart_hook\tstart\tloc=0 len=0 sql=\"SELECT $1::int\""),
            ]
        )
    }

    /// Verifies offsets for a single statement lacking a terminating semicolon.
    ///
    /// When no semicolon is present in simple query mode, Postgres leaves `len=0`.
    /// `loc=0 len=0` signals that the statement extends from index 0 through EOF.
    #[pgrx::pg_test]
    fn single_statement_no_semicolon() -> Result<()> {
        let filename = "single_statement_no_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1")?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(&"post_parse_analyze_hook\tstart\tloc=0 len=0 sql=\"SELECT 1\""),
                eq(&"planner_hook\tstart\tloc=0 len=0 sql=\"SELECT 1\""),
                eq(&"ExecutorStart_hook\tstart\tloc=0 len=0 sql=\"SELECT 1\""),
            ]
        )
    }

    /// Verifies offsets for a statement with a trailing comment but no semicolon.
    ///
    /// Because there is no terminating semicolon delimiter, Postgres leaves `len=0`.
    /// `loc=0 len=0` indicates the statement and its comment extend to EOF.
    #[pgrx::pg_test]
    fn trailing_comment_no_semicolon() -> Result<()> {
        let filename = "trailing_comment_no_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1 /* tp=1 */")?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(&"post_parse_analyze_hook\tstart\tloc=0 len=0 sql=\"SELECT 1 /* tp=1 */\""),
                eq(&"planner_hook\tstart\tloc=0 len=0 sql=\"SELECT 1 /* tp=1 */\""),
                eq(&"ExecutorStart_hook\tstart\tloc=0 len=0 sql=\"SELECT 1 /* tp=1 */\""),
            ]
        )
    }

    /// Verifies offsets when the final statement of a batch omits the semicolon.
    ///
    /// - Statement 1 has a semicolon; `loc=0 len=8` covers `SELECT 1`.
    /// - Statement 2 lacks a semicolon; `len=0` in all versions:
    ///   - v18+ `loc=10 len=0` covers `SELECT 2`.
    ///   - v13–17 `loc=9 len=0` begins just after the semicolon. Crucially, `len=0` means it goes until EOF.
    #[pgrx::pg_test]
    fn batch_final_statement_no_semicolon() -> Result<()> {
        let filename = "batch_final_statement_no_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1; SELECT 2")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2\""),
                        eq(&"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=9 len=0 sql=\"SELECT 1; SELECT 2\""),
                        eq(&"planner_hook\tstart\tloc=9 len=0 sql=\"SELECT 1; SELECT 2\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=9 len=0 sql=\"SELECT 1; SELECT 2\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2\""),
                        eq(&"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 2\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=10 len=0 sql=\"SELECT 1; SELECT 2\""),
                        eq(&"planner_hook\tstart\tloc=10 len=0 sql=\"SELECT 1; SELECT 2\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=10 len=0 sql=\"SELECT 1; SELECT 2\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets when consecutive semicolons exist in a batch.
    ///
    /// The extra semicolon produces an empty statement that invokes no hooks.
    /// - Statement 1: `loc=0 len=9` up to the first semicolon at byte 9.
    /// - Statement 2:
    ///   - v18+ `loc=12 len=8` covers `SELECT 2`.
    ///   - v13–17 `loc=11 len=9` begins just after the second semicolon.
    #[pgrx::pg_test]
    fn multiple_consecutive_semicolons() -> Result<()> {
        let filename = "multiple_consecutive_semicolons";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1 ;; SELECT 2;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=9 sql=\"SELECT 1 ;; SELECT 2;\""),
                        eq(&"planner_hook\tstart\tloc=0 len=9 sql=\"SELECT 1 ;; SELECT 2;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=9 sql=\"SELECT 1 ;; SELECT 2;\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=11 len=9 sql=\"SELECT 1 ;; SELECT 2;\""),
                        eq(&"planner_hook\tstart\tloc=11 len=9 sql=\"SELECT 1 ;; SELECT 2;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=11 len=9 sql=\"SELECT 1 ;; SELECT 2;\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=9 sql=\"SELECT 1 ;; SELECT 2;\""),
                        eq(&"planner_hook\tstart\tloc=0 len=9 sql=\"SELECT 1 ;; SELECT 2;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=9 sql=\"SELECT 1 ;; SELECT 2;\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=12 len=8 sql=\"SELECT 1 ;; SELECT 2;\""),
                        eq(&"planner_hook\tstart\tloc=12 len=8 sql=\"SELECT 1 ;; SELECT 2;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=12 len=8 sql=\"SELECT 1 ;; SELECT 2;\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies that whitespace (spaces, tabs, newlines) preceding the semicolon is included in `stmt_len`.
    ///
    /// By PostgreSQL grammar definition (`backend/parser/gram.y`), Bison assigns `@2` to the exact
    /// byte offset of the `;` token, so `stmt_location + stmt_len` always lands directly on the `;`.
    /// - Statement 1: `loc=0 len=15` covers `SELECT 1   \t\n  ` up to `;` at byte 15.
    /// - Statement 2:
    ///   - v18+: `loc=17 len=12` covers `SELECT 2 \t  ` up to `;` at byte 29.
    ///   - v13–17: `loc=16 len=13` starts right after the first semicolon, covering ` SELECT 2 \t  `.
    #[pgrx::pg_test]
    fn whitespace_before_semicolon() -> Result<()> {
        let filename = "whitespace_before_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1   \t\n  ; SELECT 2 \t  ;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=15 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                        eq(&"planner_hook\tstart\tloc=0 len=15 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=15 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=16 len=13 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                        eq(&"planner_hook\tstart\tloc=16 len=13 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=16 len=13 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=15 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                        eq(&"planner_hook\tstart\tloc=0 len=15 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=15 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=17 len=12 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                        eq(&"planner_hook\tstart\tloc=17 len=12 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=17 len=12 sql=\"SELECT 1   \\t\\n  ; SELECT 2 \\t  ;\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets when a query begins with a leading semicolon.
    ///
    /// The leading empty statement `;` is discarded without invoking any hooks.
    /// - v18+: `loc=2 len=8` covers `SELECT 1`.
    /// - v13–17: `loc=1 len=9` starts just after the first semicolon, covering ` SELECT 1`.
    #[pgrx::pg_test]
    fn leading_semicolon() -> Result<()> {
        let filename = "leading_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("; SELECT 1;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=1 len=9 sql=\"; SELECT 1;\""),
                        eq(&"planner_hook\tstart\tloc=1 len=9 sql=\"; SELECT 1;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=1 len=9 sql=\"; SELECT 1;\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=2 len=8 sql=\"; SELECT 1;\""),
                        eq(&"planner_hook\tstart\tloc=2 len=8 sql=\"; SELECT 1;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=2 len=8 sql=\"; SELECT 1;\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets when a semicolon-delimited comment sits between statements.
    ///
    /// The middle comment statement contains no SQL tokens and invokes no hooks.
    /// - Statement 1: `loc=0 len=8` in all versions.
    /// - Statement 2:
    ///   - v18+: `loc=30 len=8` covers `SELECT 2`.
    ///   - v13–17: `loc=29 len=9` starts just after the middle semicolon.
    #[pgrx::pg_test]
    fn comment_only_statement_between_statements() -> Result<()> {
        let filename = "comment_only_statement_between_statements";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1; /* comment only */; SELECT 2;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=29 len=9 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=29 len=9 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=29 len=9 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=30 len=8 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=30 len=8 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=30 len=8 sql=\"SELECT 1; /* comment only */; SELECT 2;\""
                        ),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets when a query is preceded by a multiline comment.
    ///
    /// - v18+: `loc=13 len=8` covers `SELECT 1`, excluding the multiline comment.
    /// - v13–17: `loc=0 len=21` includes the multiline comment within the slice.
    #[pgrx::pg_test]
    fn multiline_comment_leading() -> Result<()> {
        let filename = "multiline_comment_leading";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("/*\n tp=1\n */ SELECT 1;")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=21 sql=\"/*\\n tp=1\\n */ SELECT 1;\""
                        ),
                        eq(&"planner_hook\tstart\tloc=0 len=21 sql=\"/*\\n tp=1\\n */ SELECT 1;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=21 sql=\"/*\\n tp=1\\n */ SELECT 1;\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=13 len=8 sql=\"/*\\n tp=1\\n */ SELECT 1;\""
                        ),
                        eq(&"planner_hook\tstart\tloc=13 len=8 sql=\"/*\\n tp=1\\n */ SELECT 1;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=13 len=8 sql=\"/*\\n tp=1\\n */ SELECT 1;\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets when a multiline comment is inside the query before the semicolon.
    ///
    /// Because the multiline comment occurs before the terminating semicolon, all Postgres versions include it
    /// in the statement body: `loc=0 len=21` covers `SELECT 1` plus the comment.
    #[pgrx::pg_test]
    fn multiline_comment_trailing() -> Result<()> {
        let filename = "multiline_comment_trailing";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1 /*\n tp=1\n */;")?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(
                    &"post_parse_analyze_hook\tstart\tloc=0 len=21 sql=\"SELECT 1 /*\\n tp=1\\n */;\""
                ),
                eq(&"planner_hook\tstart\tloc=0 len=21 sql=\"SELECT 1 /*\\n tp=1\\n */;\""),
                eq(&"ExecutorStart_hook\tstart\tloc=0 len=21 sql=\"SELECT 1 /*\\n tp=1\\n */;\""),
            ]
        )
    }

    /// Verifies offsets for Common Table Expressions.
    ///
    /// The CTE definition and outer select form a single statement: `loc=0 len=36` covers the full
    /// 36 characters up to the semicolon in all Postgres versions.
    #[pgrx::pg_test]
    fn cte_with_opt_with_clause() -> Result<()> {
        let filename = "cte_with_opt_with_clause";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("WITH t AS (SELECT 1) SELECT * FROM t;")?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(
                    &"post_parse_analyze_hook\tstart\tloc=0 len=36 sql=\"WITH t AS (SELECT 1) SELECT * FROM t;\""
                ),
                eq(
                    &"planner_hook\tstart\tloc=0 len=36 sql=\"WITH t AS (SELECT 1) SELECT * FROM t;\""
                ),
                eq(
                    &"ExecutorStart_hook\tstart\tloc=0 len=36 sql=\"WITH t AS (SELECT 1) SELECT * FROM t;\""
                ),
            ]
        )
    }

    /// Verifies offsets and hook execution order for an EXPLAIN statement.
    ///
    /// - Outer statement: `loc=0 len=16` covers the characters up to the semicolon, dispatched through `post_parse_analyze_hook` and `ProcessUtility_hook`.
    /// - Inner statement: Handled as a sub-pipeline where Postgres sets `loc=0 len=0`.
    /// - Evolution across Postgres versions:
    ///   - v13: Only invokes `planner_hook` and `ExecutorStart_hook` for the inner query.
    ///   - v14–16: Adds `post_parse_analyze_hook` (`loc=0 len=0`) for the inner query before planning.
    ///   - v17+: Adds `ExplainOneQuery_hook` (`loc=0 len=0`) between inner parse analysis and planning.
    #[pgrx::pg_test]
    fn explain_statement() -> Result<()> {
        let filename = "explain_statement";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("EXPLAIN SELECT 1;")?;

        std::cfg_select! {
            feature = "pg13" => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=16 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"ProcessUtility_hook\tstart\tloc=0 len=16 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"planner_hook\tstart\tloc=0 len=0 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=0 sql=\"EXPLAIN SELECT 1;\""),
                    ]
                )?;
            }
            any(feature = "pg14", feature = "pg15", feature = "pg16") => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=16 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"ProcessUtility_hook\tstart\tloc=0 len=16 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=0 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"planner_hook\tstart\tloc=0 len=0 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=0 sql=\"EXPLAIN SELECT 1;\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=16 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"ProcessUtility_hook\tstart\tloc=0 len=16 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=0 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"ExplainOneQuery_hook\tstart\tloc=0 len=0 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"planner_hook\tstart\tloc=0 len=0 sql=\"EXPLAIN SELECT 1;\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=0 len=0 sql=\"EXPLAIN SELECT 1;\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets for a procedural DO block.
    ///
    /// The procedural block is processed as a utility statement without a planner stage.
    /// `loc=0 len=24` spans the characters up to the terminating semicolon in all versions.
    #[pgrx::pg_test]
    fn do_procedural_block() -> Result<()> {
        let filename = "do_procedural_block";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("DO $$ BEGIN NULL; END $$;")?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(
                    &"post_parse_analyze_hook\tstart\tloc=0 len=24 sql=\"DO $$ BEGIN NULL; END $$;\""
                ),
                eq(&"ProcessUtility_hook\tstart\tloc=0 len=24 sql=\"DO $$ BEGIN NULL; END $$;\""),
            ]
        )
    }

    /// Verifies offsets for a utility statement lacking a terminating semicolon.
    ///
    /// When no semicolon delimiter is present, Postgres leaves `len=0`.
    /// `loc=0 len=0` indicates the utility statement spans from index 0 to EOF.
    #[pgrx::pg_test]
    fn utility_statement_no_semicolon() -> Result<()> {
        let filename = "utility_statement_no_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("CREATE TEMP TABLE t_no_semi (id int)")?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(
                    &"post_parse_analyze_hook\tstart\tloc=0 len=0 sql=\"CREATE TEMP TABLE t_no_semi (id int)\""
                ),
                eq(
                    &"ProcessUtility_hook\tstart\tloc=0 len=0 sql=\"CREATE TEMP TABLE t_no_semi (id int)\""
                ),
            ]
        )
    }

    /// Verifies offsets in a mixed batch where a query is followed by a utility statement lacking a final semicolon.
    ///
    /// - Statement 1: `loc=0 len=8` terminates at the semicolon in all versions.
    /// - Statement 2: Handled by utility pipeline with `len=0`.
    ///   - v18+: `loc=10 len=0` starts at `CREATE`.
    ///   - v13–17: `loc=9 len=0` starts just after the semicolon.
    #[pgrx::pg_test]
    fn query_then_utility_no_semicolon() -> Result<()> {
        let filename = "query_then_utility_no_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("SELECT 1; CREATE TEMP TABLE t_q_u (id int)")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; CREATE TEMP TABLE t_q_u (id int)\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; CREATE TEMP TABLE t_q_u (id int)\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; CREATE TEMP TABLE t_q_u (id int)\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=9 len=0 sql=\"SELECT 1; CREATE TEMP TABLE t_q_u (id int)\""
                        ),
                        eq(
                            &"ProcessUtility_hook\tstart\tloc=9 len=0 sql=\"SELECT 1; CREATE TEMP TABLE t_q_u (id int)\""
                        ),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; CREATE TEMP TABLE t_q_u (id int)\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; CREATE TEMP TABLE t_q_u (id int)\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; CREATE TEMP TABLE t_q_u (id int)\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=10 len=0 sql=\"SELECT 1; CREATE TEMP TABLE t_q_u (id int)\""
                        ),
                        eq(
                            &"ProcessUtility_hook\tstart\tloc=10 len=0 sql=\"SELECT 1; CREATE TEMP TABLE t_q_u (id int)\""
                        ),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets in a mixed batch where a utility statement is followed by a query lacking a final semicolon.
    ///
    /// - Statement 1: `loc=0 len=32` terminates at the semicolon in all versions.
    /// - Statement 2: Traverses DML execution pipeline with `len=0`.
    ///   - v18+: `loc=34 len=0` starts at `SELECT`.
    ///   - v13–17: `loc=33 len=0` starts just after the semicolon.
    #[pgrx::pg_test]
    fn utility_then_query_no_semicolon() -> Result<()> {
        let filename = "utility_then_query_no_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("CREATE TEMP TABLE t_u_q (id int); SELECT 1")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=32 sql=\"CREATE TEMP TABLE t_u_q (id int); SELECT 1\""
                        ),
                        eq(
                            &"ProcessUtility_hook\tstart\tloc=0 len=32 sql=\"CREATE TEMP TABLE t_u_q (id int); SELECT 1\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=33 len=0 sql=\"CREATE TEMP TABLE t_u_q (id int); SELECT 1\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=33 len=0 sql=\"CREATE TEMP TABLE t_u_q (id int); SELECT 1\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=33 len=0 sql=\"CREATE TEMP TABLE t_u_q (id int); SELECT 1\""
                        ),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=32 sql=\"CREATE TEMP TABLE t_u_q (id int); SELECT 1\""
                        ),
                        eq(
                            &"ProcessUtility_hook\tstart\tloc=0 len=32 sql=\"CREATE TEMP TABLE t_u_q (id int); SELECT 1\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=34 len=0 sql=\"CREATE TEMP TABLE t_u_q (id int); SELECT 1\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=34 len=0 sql=\"CREATE TEMP TABLE t_u_q (id int); SELECT 1\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=34 len=0 sql=\"CREATE TEMP TABLE t_u_q (id int); SELECT 1\""
                        ),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets in a transaction batch where the final COMMIT lacks a semicolon.
    ///
    /// - `BEGIN`: `loc=0 len=5`.
    /// - `SELECT 1`: v18+ `loc=7 len=8`; v13–17 `loc=6 len=9`.
    /// - `COMMIT`: Handled by `ProcessUtility_hook` with `len=0`.
    #[pgrx::pg_test]
    fn transaction_commands_no_final_semicolon() -> Result<()> {
        let filename = "transaction_commands_no_final_semicolon";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        client.simple_query("BEGIN; SELECT 1; COMMIT")?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=5 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"ProcessUtility_hook\tstart\tloc=0 len=5 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=6 len=9 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"planner_hook\tstart\tloc=6 len=9 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=6 len=9 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=16 len=0 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"ProcessUtility_hook\tstart\tloc=16 len=0 sql=\"BEGIN; SELECT 1; COMMIT\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(&"post_parse_analyze_hook\tstart\tloc=0 len=5 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"ProcessUtility_hook\tstart\tloc=0 len=5 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=7 len=8 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"planner_hook\tstart\tloc=7 len=8 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"ExecutorStart_hook\tstart\tloc=7 len=8 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"post_parse_analyze_hook\tstart\tloc=17 len=0 sql=\"BEGIN; SELECT 1; COMMIT\""),
                        eq(&"ProcessUtility_hook\tstart\tloc=17 len=0 sql=\"BEGIN; SELECT 1; COMMIT\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies offsets in a multi-statement batch when an intermediate statement fails during planning.
    ///
    /// - Statement 1 completes normally (`loc=0 len=8`).
    /// - Statement 2 enters `post_parse_analyze_hook` and `planner_hook`, then constant folding raises division by zero.
    /// - Statement 2 never enters `ExecutorStart_hook`, and Statement 3 is never reached.
    #[pgrx::pg_test]
    fn multi_statement_batch_planning_error_offsets() -> Result<()> {
        let filename = "multi_statement_batch_planning_error_offsets";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        assert_that!(
            client.simple_query("SELECT 1; SELECT 1 / 0; SELECT 2;"),
            err(anything())
        );

        client.check_connection()?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / 0; SELECT 2;\""
                        ),
                        eq(&"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / 0; SELECT 2;\""),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / 0; SELECT 2;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=9 len=13 sql=\"SELECT 1; SELECT 1 / 0; SELECT 2;\""
                        ),
                        eq(&"planner_hook\tstart\tloc=9 len=13 sql=\"SELECT 1; SELECT 1 / 0; SELECT 2;\""),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / 0; SELECT 2;\""
                        ),
                        eq(&"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / 0; SELECT 2;\""),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / 0; SELECT 2;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=10 len=12 sql=\"SELECT 1; SELECT 1 / 0; SELECT 2;\""
                        ),
                        eq(&"planner_hook\tstart\tloc=10 len=12 sql=\"SELECT 1; SELECT 1 / 0; SELECT 2;\""),
                    ]
                )?;
            }
        }

        Ok(())
    }

    /// Verifies that a syntax error anywhere in a multi-statement batch prevents all statements from entering any pipeline hook.
    ///
    /// When `raw_parser` fails on an invalid token in any statement, the entire query string is rejected
    /// before `post_parse_analyze_hook`, `planner_hook`, or `ExecutorStart_hook` can execute.
    #[pgrx::pg_test]
    fn multi_statement_batch_syntax_error_bypasses_all_pipelines() -> Result<()> {
        let filename = "multi_statement_batch_syntax_error_bypasses_all_pipelines";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        assert_that!(
            client.simple_query("SELECT 1; blah; SELECT 2;"),
            err(anything())
        );

        client.check_connection()?;

        verify_that!(pipeline_starts(filename), is_empty())
    }

    /// Verifies offsets in a multi-statement batch when an intermediate statement fails during parse analysis.
    ///
    /// - Statement 1 completes normally (`loc=0 len=8`).
    /// - Statement 2 fails during `parse_analyze`, so it never enters `post_parse_analyze_hook`, `planner_hook`, or `ExecutorStart_hook`.
    /// - Statement 3 is never reached.
    #[pgrx::pg_test]
    fn multi_statement_batch_parse_error_offsets() -> Result<()> {
        let filename = "multi_statement_batch_parse_error_offsets";
        let mut client = super::setup_test(filename);
        super::clear_trace(filename);

        assert_that!(
            client.simple_query("SELECT 1; SELECT * FROM non_existent_table; SELECT 2;"),
            err(anything())
        );

        client.check_connection()?;

        verify_that!(
            pipeline_starts(filename),
            elements_are![
                eq(
                    &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT * FROM non_existent_table; SELECT 2;\""
                ),
                eq(
                    &"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT * FROM non_existent_table; SELECT 2;\""
                ),
                eq(
                    &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT * FROM non_existent_table; SELECT 2;\""
                ),
            ]
        )
    }

    /// Verifies offsets in a multi-statement batch when an intermediate statement fails at runtime.
    ///
    /// - Statement 1 completes normally (`loc=0 len=8`).
    /// - Statement 2 enters `post_parse_analyze_hook`, `planner_hook`, and `ExecutorStart_hook` before failing during `ExecutorRun`.
    /// - Statement 3 is never reached.
    #[pgrx::pg_test]
    fn multi_statement_batch_runtime_error_offsets() -> Result<()> {
        let filename = "multi_statement_batch_runtime_error_offsets";
        let mut client = super::setup_test(filename);

        client.simple_query("CREATE TEMP TABLE t_err_offsets (value) AS VALUES (0)")?;
        super::clear_trace(filename);

        assert_that!(
            client.simple_query("SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;"),
            err(anything())
        );

        client.check_connection()?;

        std::cfg_select! {
            any(
                feature = "pg13",
                feature = "pg14",
                feature = "pg15",
                feature = "pg16",
                feature = "pg17",
            ) => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=9 len=36 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=9 len=36 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=9 len=36 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                    ]
                )?;
            }
            _ => {
                verify_that!(
                    pipeline_starts(filename),
                    elements_are![
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=0 len=8 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                        eq(
                            &"post_parse_analyze_hook\tstart\tloc=10 len=35 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                        eq(
                            &"planner_hook\tstart\tloc=10 len=35 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                        eq(
                            &"ExecutorStart_hook\tstart\tloc=10 len=35 sql=\"SELECT 1; SELECT 1 / value FROM t_err_offsets; SELECT 2;\""
                        ),
                    ]
                )?;
            }
        }

        Ok(())
    }
}
