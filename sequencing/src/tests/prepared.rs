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
}
