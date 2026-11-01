// SPDX-License-Identifier: MIT

use super::*;

#[pgrx::pg_schema]
mod tests {
    use googletest::prelude::*;

    #[pgrx::pg_test]
    fn prepared_statement_execution_reuses_plan() -> Result<()> {
        let filename = "prepared_statement_execution_reuses_plan";
        let mut client = super::setup_test(filename);

        // Force immediate plan caching, bypassing PostgreSQL's default "auto" 5-execution threshold
        client.simple_query("SET plan_cache_mode = force_generic_plan")?;
        super::clear_trace(filename);

        client.simple_query(
            "PREPARE p_seq AS SELECT 1; EXECUTE p_seq; EXECUTE p_seq; DEALLOCATE p_seq",
        )?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // LOG: statement: PREPARE + EXECUTE + EXECUTE + DEALLOCATE
                // PREPARE
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "end")),
                // EXECUTE; first run includes planner
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
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
                eq(&("ProcessUtility_hook", "end")),
                // EXECUTE; subsequent runs exclude planner (after generic plan)
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
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
                eq(&("ProcessUtility_hook", "end")),
                // DEALLOCATE
                eq(&("post_parse_analyze_hook", "start")),
                eq(&("post_parse_analyze_hook", "end")),
                eq(&("ProcessUtility_hook", "start")),
                eq(&("ProcessUtility_hook", "end")),
                eq(&("xact_callback", "PRE_COMMIT")),
                eq(&("xact_callback", "COMMIT")),
            ]
        )
    }

    #[derive(Debug)]
    struct InvalidInt4;

    impl postgres::types::ToSql for InvalidInt4 {
        fn accepts(_ty: &postgres::types::Type) -> bool {
            true
        }

        fn encode_format(&self, _ty: &postgres::types::Type) -> postgres::types::Format {
            postgres::types::Format::Text
        }

        fn to_sql(
            &self,
            _ty: &postgres::types::Type,
            out: &mut postgres::types::private::BytesMut,
        ) -> std::result::Result<postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>>
        {
            out.extend_from_slice(b"not_an_int");
            Ok(postgres::types::IsNull::No)
        }

        postgres::types::to_sql_checked!();
    }

    #[pgrx::pg_test]
    fn extended_query_bind_error_aborts_before_executor() -> Result<()> {
        let filename = "extended_query_bind_error_aborts_before_executor";
        let mut client = super::setup_test(filename);

        let stmt = client.prepare("SELECT $1::int")?;
        super::clear_trace(filename);

        assert_that!(client.query(&stmt, &[&InvalidInt4]), err(anything()));

        client.check_connection()?;

        verify_that!(
            super::read_trace(filename),
            elements_are![
                eq(&("emit_log_hook", "entry")), // ERROR: invalid input syntax for type integer: "not_an_int"
                eq(&("xact_callback", "ABORT")),
            ]
        )
    }
}
