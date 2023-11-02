/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include <time.h>

#include "postgres.h"
#include "executor/executor.h"
#include "miscadmin.h"
#include "tcop/utility.h"

#if PG_VERSION_NUM >= 150000
#include "common/pg_prng.h"
#else
#include <stdlib.h>
#endif

#include "pg_otel.h"
#include "pg_otel_ipc.h"
#include "pg_otel_traces.h"

static struct otelTraceBatch *otel_AddTraceBatch(struct otelTraceExporter *);
static void otel_AddTraceResource(struct otelTraceBatch *, struct otelResource *);

static struct otelSpan *
otel_StartSpan(MemoryContext ctx, const struct otelSpan *parent,
			   const PlannedStmt *planned, const char *statement)
{
	bool needsTrace = true;
	struct otelSpan *s;
	struct timespec ts;

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

	/*
	 * Copy trace ID, parent ID, and propagated trace state from one of three
	 * sources:
	 *  1. The parent span, if it exists
	 *  2. Propagation values in the session, if they exist
	 *  3. TODO: SQL comments
	 */
	if (parent != NULL)
	{
		memcpy(s->trace, parent->trace, sizeof(parent->trace));
		memcpy(s->parent, parent->id, sizeof(parent->id));
		s->span.parent_span_id.data = s->parent;
		s->span.parent_span_id.len = sizeof(s->parent);
		s->span.trace_state = parent->span.trace_state;

		needsTrace = false;
	}
	else if (config.traceContext.parsed)
	{
		memcpy(s->trace, config.traceContext.traceID, sizeof(s->trace));
		memcpy(s->parent, config.traceContext.parentID, sizeof(s->parent));
		s->span.parent_span_id.data = s->parent;
		s->span.parent_span_id.len = sizeof(s->parent);

		if (config.traceContext.textTracestate != NULL &&
			config.traceContext.textTracestate[0] != '\0')
			s->span.trace_state = config.traceContext.textTracestate;

		needsTrace = false;
	}
	else if (statement != NULL && statement[0] != '\0')
	{
		// TODO: Extract {traceparent} from statement comment
		// - https://google.github.io/sqlcommenter/

		fprintf(stderr, PG_OTEL_LIBRARY ": start -- (%d, %d)\n%s\n",
				planned->stmt_location, planned->stmt_len, statement);
	}

	/* Generate a trace ID (root span) when none was present above */
	if (needsTrace)
	{
#if PG_VERSION_NUM >= 150000
		((uint64_t *) s->trace)[0] = pg_prng_uint64(&pg_global_prng_state);
#else
		((uint32_t *) s->trace)[0] = ((uint32_t) random());
		((uint32_t *) s->trace)[1] = ((uint32_t) random());
#endif
		((uint64_t *) s->trace)[1] = *((uint64_t *) s->id);
	}

	s->span.start_time_unix_nano = ts.tv_sec * 1000000000 + ts.tv_nsec;

	return s;
}

static void
otel_EndQuerySpan(struct otelSpan *s, const QueryDesc *query)
{
	const char *operation;
	PlannedStmt *planned = query->plannedstmt;
	struct timespec ts;

	/* Get the current time right away */
	clock_gettime(CLOCK_REALTIME, &ts);

	/* See: tcop/pquery.c */
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

		operation = GetCommandTagName(tag);
		otel_SpanAttributeStr(s, "db.operation", operation);
	}

	if (planned->queryId != 0)
		otel_SpanAttributeInt(s, "db.postgresql.query_id", planned->queryId);

	if (query->estate != NULL)
		otel_SpanAttributeInt(s, "db.postgresql.rows", query->estate->es_processed);

	if (MyProcPid != 0) /* miscadmin.h */
		otel_SpanAttributeInt(s, "process.pid", MyProcPid);

	if (MyProcPort != NULL) /* miscadmin.h */
	{
		if (MyProcPort->database_name != NULL)
			otel_SpanAttributeStr(s, "db.name", MyProcPort->database_name);

		if (MyProcPort->user_name != NULL)
			otel_SpanAttributeStr(s, "db.user", MyProcPort->user_name);

		/* TODO: MyProcPort->remote_host + MyProcPort->remote_port */
	}

	s->span.end_time_unix_nano = ts.tv_sec * 1000000000 + ts.tv_nsec;
	s->span.kind = true ? OTEL_SPAN_KIND(SERVER) : OTEL_SPAN_KIND(INTERNAL);

	if (s->span.name == NULL || s->span.name[0] == '\0')
		s->span.name = (char *) operation;

	//s.span.trace_state;		// char *
	//s.span.status = &s.status;	// optional
}

static void
otel_EndUtilitySpan(struct otelSpan *s, ProcessUtilityContext context,
					const PlannedStmt *planned)
{
	const char *operation;
	struct timespec ts;

	/* Get the current time right away */
	clock_gettime(CLOCK_REALTIME, &ts);

	operation = GetCommandTagName(CreateCommandTag(planned->utilityStmt));
	otel_SpanAttributeStr(s, "db.operation", operation);

	if (planned->queryId != 0)
		otel_SpanAttributeInt(s, "db.postgresql.query_id", planned->queryId);

	s->span.end_time_unix_nano = ts.tv_sec * 1000000000 + ts.tv_nsec;

	if (context == PROCESS_UTILITY_TOPLEVEL)
		s->span.kind = OTEL_SPAN_KIND(SERVER);
	else
		s->span.kind = OTEL_SPAN_KIND(INTERNAL);

	if (s->span.name == NULL || s->span.name[0] == '\0')
		s->span.name = (char *) operation;

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

/*
 * Called by the background worker to put a span in the exporter queue.
 */
static void
otel_ReceiveSpan(struct otelTraceExporter *exporter,
				 const uint8_t *packed, size_t size)
{
	struct otelTraceBatch *batch;
	OTEL_TYPE_TRACE(ResourceSpans) *resourceSpans;
	OTEL_TYPE_TRACE(Span) *span;

	if (dlist_is_empty(&exporter->queue))
		batch = otel_AddTraceBatch(exporter);
	else
		batch = dlist_tail_element(struct otelTraceBatch, list_node,
								   &exporter->queue);

	if (exporter->queueLength >= exporter->queueMax)
		batch->dropped++;
	else
	{
		/* unpack returns NULL when it cannot unpack the message */
		span = OTEL_FUNC_TRACE(span__unpack)(&batch->allocator, size, packed);

		if (span == NULL)
			batch->dropped++;
		else
		{
			if (batch->length >= batch->capacity)
				batch = otel_AddTraceBatch(exporter);

			batch->spans[batch->length] = span;
			batch->length++;
			exporter->queueLength++;

			resourceSpans = llast(batch->resourceSpans);
			resourceSpans->scope_spans[0]->n_spans++;
		}
	}
}

/*
 * Allocate a batch in its own top-level context and append it to exporter's queue.
 */
static struct otelTraceBatch *
otel_AddTraceBatch(struct otelTraceExporter *exporter)
{
	MemoryContext ctx = AllocSetContextCreate(NULL, /* parent */
											  PG_OTEL_LIBRARY " trace batch",
											  ALLOCSET_START_SMALL_SIZES);
	struct otelTraceBatch *batch = MemoryContextAllocZero(ctx, sizeof(*batch));

	batch->capacity = exporter->batchMax;
	batch->context = ctx;
	batch->spans = MemoryContextAlloc(batch->context,
									  sizeof(*(batch->spans)) *
									  batch->capacity);

	otel_InitProtobufCAllocator(&batch->allocator, batch->context);
	otel_AddTraceResource(batch, &exporter->resource);

	dlist_push_tail(&exporter->queue, &batch->list_node);

	return batch;
}

/*
 * Store a copy of resource in batch to be exported with any following spans.
 */
static void
otel_AddTraceResource(struct otelTraceBatch *batch, struct otelResource *resource)
{
	MemoryContext oldContext = MemoryContextSwitchTo(batch->context);
	OTEL_TYPE_TRACE(ResourceSpans)  data;
	OTEL_TYPE_TRACE(ResourceSpans) *next;
	OTEL_TYPE_TRACE(ScopeSpans)     scopeSpansData;
	OTEL_TYPE_TRACE(ScopeSpans)    *scopeSpansList[1] = { &scopeSpansData };
	uint8_t *packed;
	size_t size;

	/* Prepare a ResourceSpans object containing a list of one ScopeSpans */
	OTEL_FUNC_TRACE(resource_spans__init)(&data);
	OTEL_FUNC_TRACE(scope_spans__init)(&scopeSpansData);
	data.resource = &resource->resource;
	data.scope_spans = scopeSpansList;
	data.n_scope_spans = 1;

	/* Copy it into the MemoryContext of the batch */
	packed = palloc(OTEL_FUNC_TRACE(resource_spans__get_packed_size)(&data));
	size = OTEL_FUNC_TRACE(resource_spans__pack)(&data, packed);
	next = OTEL_FUNC_TRACE(resource_spans__unpack)(&batch->allocator, size, packed);
	pfree(packed);

	/* Point that copy at the current position in the list of Spans */
	next->scope_spans[0]->spans = batch->spans + batch->length;
	next->scope_spans[0]->n_spans = 0;

	batch->resourceSpans = lappend(batch->resourceSpans, next);

	MemoryContextSwitchTo(oldContext);
}

/*
 * Send request to the collector configured in exporter, ignoring any errors.
 */
static void
otel_SendTraceRequestToCollector(MemoryContext ctx, CURL *http,
								struct otelTraceExporter *exporter,
								OTEL_TYPE_EXPORT_TRACE(Request) *request)
{
	uint8_t *body = NULL;
	char     httpErrorBuffer[CURL_ERROR_SIZE];
	CURLcode result;
	size_t   size;

	struct curl_slist *headers = NULL;

	size = OTEL_FUNC_EXPORT_TRACE(request__get_packed_size)(request);
	body = MemoryContextAlloc(ctx, size);
	size = OTEL_FUNC_EXPORT_TRACE(request__pack)(request, body);

	/* These do not error or their error can be ignored */
	curl_easy_reset(http);
	curl_easy_setopt(http, CURLOPT_ERRORBUFFER, httpErrorBuffer);
	curl_easy_setopt(http, CURLOPT_CONNECTTIMEOUT_MS, 1 + (exporter->timeoutMS / 2));
	curl_easy_setopt(http, CURLOPT_TIMEOUT_MS, exporter->timeoutMS);
	curl_easy_setopt(http, CURLOPT_USERAGENT, PG_OTEL_USERAGENT);

	if (exporter->insecure)
	{
		curl_easy_setopt(http, CURLOPT_SSL_VERIFYHOST, 0);
		curl_easy_setopt(http, CURLOPT_SSL_VERIFYPEER, 0);
	}

	/* TODO: check errors */
	result = curl_easy_setopt(http, CURLOPT_URL, exporter->endpoint);

	/* TODO: check errors */
	headers = curl_slist_append(headers, PG_OTEL_HEADER_PROTOBUF);

	curl_easy_setopt(http, CURLOPT_HTTPHEADER, headers);

	/*
	 * TODO: gzip encoding
	 * TODO: retry and backoff
	 * - https://opentelemetry.io/docs/specs/otlp/
	 * - https://opentelemetry.io/docs/specs/otel/protocol/exporter/
	 */

#ifdef PG_OTEL_DEBUG
	/* Print debugging information to stderr; off by default */
	curl_easy_setopt(http, CURLOPT_VERBOSE, 1);
#endif

	curl_easy_setopt(http, CURLOPT_POST, 1);
	curl_easy_setopt(http, CURLOPT_POSTFIELDS, body);
	curl_easy_setopt(http, CURLOPT_POSTFIELDSIZE, size);

	result = curl_easy_perform(http);

	/* TODO: check errors */
	result = result;

	curl_slist_free_all(headers);
	curl_easy_reset(http);
	pfree(body);
}

/*
 * Called by the background worker to send a batch to the collector.
 */
static void
otel_SendSpansToCollector(struct otelTraceExporter *exporter, CURL *http)
{
	struct otelTraceBatch *batch;
	OTEL_TYPE_EXPORT_TRACE(Request) request;
	OTEL_TYPE_COMMON(InstrumentationScope) scopeData;
	ListCell *cell;

	if (dlist_is_empty(&exporter->queue))
		return;

	OTEL_FUNC_EXPORT_TRACE(request__init)(&request);
	OTEL_FUNC_COMMON(instrumentation_scope__init)(&scopeData);

	/*
	 * All spans come from the same instrumentation scope: this module.
	 * - https://opentelemetry.io/docs/specs/otel/glossary/#instrumentation-scope
	 */
	scopeData.name = PG_OTEL_LIBRARY;
	scopeData.version = PG_OTEL_VERSION;

	batch = dlist_head_element(struct otelTraceBatch, list_node,
							   &exporter->queue);

	request.n_resource_spans = batch->resourceSpans->length;
	request.resource_spans = MemoryContextAlloc(batch->context,
												sizeof(*request.resource_spans) *
												request.n_resource_spans);

	cell = list_head(batch->resourceSpans);
	for (int i = 0; i < request.n_resource_spans; i++)
	{
		request.resource_spans[i] = lfirst(cell);
		request.resource_spans[i]->schema_url = PG_OTEL_SCHEMA;
		request.resource_spans[i]->scope_spans[0]->schema_url = PG_OTEL_SCHEMA;
		request.resource_spans[i]->scope_spans[0]->scope = &scopeData;
#if PG_VERSION_NUM >= 130000
		cell = lnext(batch->resourceSpans, cell);
#else
		cell = lnext(cell);
#endif
	}

	otel_SendTraceRequestToCollector(batch->context, http, exporter, &request);

	dlist_pop_head_node(&exporter->queue);
	exporter->queueLength -= batch->length;

	MemoryContextDelete(batch->context);
}

static void
otel_InitTraceExporter(struct otelTraceExporter *exporter,
					   const struct otelConfiguration *config)
{
	dlist_init(&exporter->queue);
	exporter->endpoint = NULL;
	exporter->queueLength = 0;

	otel_InitResource(&exporter->resource);
	otel_LoadTraceConfig(exporter, config);
}

/*
 * Called by the background worker when PostgreSQL configuration changes.
 */
static void
otel_LoadTraceConfig(struct otelTraceExporter *exporter,
					 const struct otelConfiguration *config)
{
	otel_LoadResource(config, &exporter->resource);

	/*
	 * Per-signal URLs MUST be used as-is without any modification. When there
	 * is no path, append the root path.
	 *
	 * Without a per-signal configuration, the OTLP endpoint is a base URL and
	 * signals are sent relative to that.
	 *
	 * - https://opentelemetry.io/docs/specs/otel/protocol/exporter/
	 */
	{
		StringInfoData str;

		Assert(config->otlpTrace.endpoint == NULL); /* TODO: per-signal */

		initStringInfo(&str);
		appendStringInfoString(&str, config->otlp.endpoint);

		if (!pg_str_endswith(config->otlp.endpoint, "/"))
			appendStringInfoString(&str, "/");

		appendStringInfoString(&str, "v1/traces");

		if (exporter->endpoint)
			pfree(exporter->endpoint);
		exporter->endpoint = str.data;
	}

	{
		Assert(config->otlpTrace.timeoutMS == 0); /* TODO: per-signal */

		exporter->timeoutMS = config->otlp.timeoutMS;
	}

	/*
	 * TODO: Accept settings for "Batch Span Processor"
	 * - https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/
	 *
	 * TODO: Clamp both >= 1 for now.
	 */
	exporter->batchMax = 512;
	exporter->queueMax = 2048;

	exporter->insecure = false;
}
