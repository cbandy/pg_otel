/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include <time.h>

#include "postgres.h"
#include "executor/executor.h"
#include "tcop/utility.h"

#if PG_VERSION_NUM >= 150000
#include "common/pg_prng.h"
#else
#include <stdlib.h>
#endif

#include "pg_otel.h"
#include "pg_otel_ipc.h"
#include "pg_otel_proto.h"

static struct otelSpan *
otel_StartSpan(MemoryContext ctx,
			   const struct otelSpan *parent, const char *statement)
{
	struct otelSpan *s;
	struct timespec ts;
	uint8_t *traceID = NULL;

	/* Get the current time right away */
	clock_gettime(CLOCK_REALTIME, &ts);

	s = MemoryContextAllocZero(ctx, sizeof(*s));
	otel_InitSpan(s);

#if PG_VERSION_NUM >= 150000
	*((uint64_t *) s->id) = pg_prng_uint64_range(&pg_global_prng_state, 1, PG_UINT64_MAX);
#else
	((uint32_t *) s->id)[0] = ((uint32_t) random());
	((uint32_t *) s->id)[1] = ((uint32_t) random()) + 1;
#endif

	if (parent != NULL)
	{
		memcpy(s->trace, parent->trace, sizeof(parent->trace));
		traceID = s->trace;

		memcpy(s->parent, parent->id, sizeof(parent->id));
		s->span.parent_span_id.data = s->parent;
		s->span.parent_span_id.len = sizeof(s->parent);
	}

	//if (traceID == NULL && false)
	//{
	//	// TODO: session configuration for trace id
	//}
	if (traceID == NULL && statement != NULL && statement[0] != '\0')
	{
		// TODO: Extract {traceparent} from statement comment
	}
	if (traceID == NULL)
	{
#if PG_VERSION_NUM >= 150000
		((uint64_t *) s->trace)[0] = pg_prng_uint64(&pg_global_prng_state);
#else
		((uint32_t *) s->trace)[0] = ((uint32_t) random());
		((uint32_t *) s->trace)[1] = ((uint32_t) random());
#endif
		((uint64_t *) s->trace)[1] = *((uint64_t *) s->id);
		traceID = s->trace;
	}

	s->span.start_time_unix_nano = ts.tv_sec * 1000000000 + ts.tv_nsec;

	return s;
}

static void
otel_EndQuerySpan(struct otelSpan *s, const QueryDesc *query)
{
	PlannedStmt *planned = query->plannedstmt;
	struct timespec ts;

	/* Get the current time right away */
	clock_gettime(CLOCK_REALTIME, &ts);

	if (planned->queryId != 0)
		otel_SpanAttributeInt(s, "db.postgresql.query_id", planned->queryId);

	s->span.end_time_unix_nano = ts.tv_sec * 1000000000 + ts.tv_nsec;
	s->span.kind = true ? OTEL_SPAN_KIND(SERVER) : OTEL_SPAN_KIND(INTERNAL);

	/*
	 * Set the span name according to the query operation.
	 * See: tcop/pquery.c
	 */
	if (s->span.name == NULL || s->span.name[0] == '\0')
	{
		CommandTag tag = CMDTAG_UNKNOWN;

		switch (query->operation)
		{
			case CMD_SELECT: tag = CMDTAG_SELECT; break;
			case CMD_INSERT: tag = CMDTAG_INSERT; break;
			case CMD_UPDATE: tag = CMDTAG_UPDATE; break;
			case CMD_DELETE: tag = CMDTAG_DELETE; break;
#if PG_VERSION_NUM >= 150000
			case CMD_MERGE: tag = CMDTAG_MERGE; break;
#endif
			case CMD_UTILITY:
				tag = CreateCommandTag(planned->utilityStmt);
				break;
			case CMD_NOTHING:
			case CMD_UNKNOWN:
				tag = CMDTAG_UNKNOWN;
		}

		s->span.name = (char *) GetCommandTagName(tag);
	}

	//s.span.trace_state;		// char *
	//s.span.status = &s.status;	// optional
}

static void
otel_EndUtilitySpan(struct otelSpan *s, ProcessUtilityContext context,
					const PlannedStmt *planned)
{
	struct timespec ts;

	/* Get the current time right away */
	clock_gettime(CLOCK_REALTIME, &ts);

	if (planned->queryId != 0)
		otel_SpanAttributeInt(s, "db.postgresql.query_id", planned->queryId);

	s->span.end_time_unix_nano = ts.tv_sec * 1000000000 + ts.tv_nsec;

	if (context == PROCESS_UTILITY_TOPLEVEL)
		s->span.kind = OTEL_SPAN_KIND(SERVER);
	else
		s->span.kind = OTEL_SPAN_KIND(INTERNAL);

	s->span.name = (char *) GetCommandTagName(CreateCommandTag(planned->utilityStmt));

	//s.span.trace_state;		// char *
	//s.span.status = &s.status;	// optional
}

/*
 * Called by backends to send one span to the background worker.
 */
static void
otel_SendSpan(struct otelIPC *ipc, const struct otelSpan *s)
{
	uint8_t *packed = palloc(OTEL_FUNC_TRACE(span__get_packed_size)(&s->span));
	size_t size = OTEL_FUNC_TRACE(span__pack)(&s->span, packed);

	otel_SendOverIPC(ipc, PG_OTEL_IPC_TRACES, packed, size);

	pfree(packed);
}
