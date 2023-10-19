/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include <sys/time.h>

#include "postgres.h"
#include "common/string.h"
#include "lib/stringinfo.h"
#include "libpq/libpq-be.h"
#include "miscadmin.h"
#include "tcop/tcopprot.h"
#include "utils/elog.h"
#include "utils/memutils.h"

#if PG_VERSION_NUM >= 140000
#include "utils/wait_event.h"
#else
#include "pgstat.h"
#endif

#include "pg_otel.h"
#include "pg_otel_ipc.h"
#include "pg_otel_logs.h"

static struct otelLogsBatch *otel_AddLogsBatch(struct otelLogsExporter *);
static void otel_AddLogsResource(struct otelLogsBatch *, struct otelResource *);

/*
 * Called by backends to send one log message to the background worker.
 */
static void
otel_SendLogMessage(struct otelIPC *ipc, const ErrorData *edata)
{
	struct otelLogRecord r;
	struct timeval tv;
	uint64_t unixNanoSec;

	gettimeofday(&tv, NULL);
	unixNanoSec = tv.tv_sec * 1000000000 + tv.tv_usec * 1000;

	otel_InitLogRecord(&r);

	r.record.body->string_value = edata->message;
	r.record.body->value_case = OTEL_VALUE_CASE(STRING);
	r.record.observed_time_unix_nano = unixNanoSec;
	r.record.time_unix_nano = unixNanoSec;

	/*
	 * Set severity number and text according to OpenTelemetry Log Data Model
	 * and error_severity() in elog.c.
	 * - https://docs.opentelemetry.io/reference/specification/logs/data-model/
	 *
	 * > ["SeverityText"] is the original string representation of the severity
	 * > as it is known at the source.
	 *
	 * > If "SeverityNumber" is present and has a value of ERROR (numeric 17)
	 * > or higher then it is an indication that the log record represents an
	 * > erroneous situation.
	 *
	 * > If the log record represents a non-erroneous event the "SeverityNumber"
	 * > field … may be set to any numeric value less than ERROR (numeric 17).
	 *
	 * > Smaller numerical values correspond to less severe events (such as
	 * > debug events), larger numerical values correspond to more severe
	 * > events (such as errors and critical events).
	 *
	 * > If the source format has only a single severity that matches the
	 * > meaning of the range then it is recommended to assign that severity
	 * > the smallest value of the range.
	 */
	switch (edata->elevel)
	{
		case DEBUG5:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(TRACE);
			r.record.severity_text = "DEBUG";
			break;
		case DEBUG4:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(TRACE2);
			r.record.severity_text = "DEBUG";
			break;
		case DEBUG3:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(TRACE3);
			r.record.severity_text = "DEBUG";
			break;
		case DEBUG2:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(TRACE4);
			r.record.severity_text = "DEBUG";
			break;
		case DEBUG1:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(DEBUG);
			r.record.severity_text = "DEBUG";
			break;
		case LOG:
		case LOG_SERVER_ONLY:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(INFO);
			r.record.severity_text = "LOG";
			break;
		case INFO:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(INFO);
			r.record.severity_text = "INFO";
			break;
		case NOTICE:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(INFO2);
			r.record.severity_text = "NOTICE";
			break;
		case WARNING:
#if PG_VERSION_NUM >= 140000
		case WARNING_CLIENT_ONLY:
			/*
			 * The log hook is not called for WARNING_CLIENT_ONLY, but
			 * it is included here for completeness.
			 */
#endif
			r.record.severity_number = OTEL_SEVERITY_NUMBER(WARN);
			r.record.severity_text = "WARNING";
			break;
		case ERROR:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(ERROR);
			r.record.severity_text = "ERROR";
			break;
		case FATAL:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(FATAL);
			r.record.severity_text = "FATAL";
			break;
		case PANIC:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(FATAL2);
			r.record.severity_text = "PANIC";
			break;
		default:
			r.record.severity_number = OTEL_SEVERITY_NUMBER(FATAL2);
	}

	/*
	 * Set attributes according to OpenTelemetry Semantic Conventions.
	 * - https://opentelemetry.io/docs/specs/otel/semantic-conventions/
	 */

	if (MyProcPid != 0) /* miscadmin.h */
		otel_LogAttributeInt(&r, "process.pid", MyProcPid);

	if (edata->funcname != NULL)
		otel_LogAttributeStr(&r, "code.function", edata->funcname);

	if (edata->filename != NULL)
	{
		otel_LogAttributeStr(&r, "code.filepath", edata->filename);
		otel_LogAttributeInt(&r, "code.lineno", edata->lineno);
	}

	if (MyProcPort != NULL) /* miscadmin.h */
	{
		if (MyProcPort->database_name != NULL)
			otel_LogAttributeStr(&r, "db.name", MyProcPort->database_name);

		if (MyProcPort->user_name != NULL)
			otel_LogAttributeStr(&r, "db.user", MyProcPort->user_name);

		/* TODO: MyProcPort->remote_host + MyProcPort->remote_port */
	}

	if (debug_query_string != NULL && !edata->hide_stmt) /* tcopprot.h */
	{
		otel_LogAttributeStr(&r, "db.statement", debug_query_string);

		if (edata->cursorpos > 0)
			otel_LogAttributeInt(&r, "db.postgresql.cursor_position", edata->cursorpos);
	}

	if (edata->internalquery != NULL)
	{
		otel_LogAttributeStr(&r, "db.postgresql.internal_query", edata->internalquery);

		if (edata->internalpos > 0)
			otel_LogAttributeInt(&r, "db.postgresql.internal_position", edata->internalpos);
	}

	if (edata->context != NULL && !edata->hide_ctx)
		otel_LogAttributeStr(&r, "db.postgresql.context", edata->context);

	if (edata->sqlerrcode != 0)
		otel_LogAttributeStr(&r, "db.postgresql.state_code", unpack_sql_state(edata->sqlerrcode));

	if (edata->hint != NULL)
		otel_LogAttributeStr(&r, "db.postgresql.hint", edata->hint);

	if (edata->detail_log != NULL)
		otel_LogAttributeStr(&r, "db.postgresql.detail", edata->detail_log);

	else if (edata->detail != NULL)
		otel_LogAttributeStr(&r, "db.postgresql.detail", edata->detail);

	if (application_name != NULL && application_name[0] != '\0') /* guc.h */
		otel_LogAttributeStr(&r, "db.postgresql.application_name", application_name);

	/*
	 * TODO: backend_type
	 * TODO: session_id
	 * TODO: vxid + txid
	 * TODO: leader_pid
	 * TODO: query_id
	 */

	{
		uint8_t *packed = palloc(OTEL_FUNC_LOGS(log_record__get_packed_size)(&r.record));
		size_t size = OTEL_FUNC_LOGS(log_record__pack)(&r.record, packed);

		otel_SendOverIPC(ipc, PG_OTEL_IPC_LOGS, packed, size);

		pfree(packed);
	}
}

/*
 * Called by the background worker to put a log message in the exporter queue.
 */
static void
otel_ReceiveLogMessage(struct otelLogsExporter *exporter,
					   const uint8_t *packed, size_t size)
{
	struct otelLogsBatch *batch;
	OTEL_TYPE_LOGS(LogRecord) *record;
	OTEL_TYPE_LOGS(ResourceLogs) *resourceLogs;

	if (dlist_is_empty(&exporter->queue))
		batch = otel_AddLogsBatch(exporter);
	else
		batch = dlist_tail_element(struct otelLogsBatch, list_node,
								   &exporter->queue);

	if (exporter->queueLength >= exporter->queueMax)
		batch->dropped++;
	else
	{
		/* unpack returns NULL when it cannot unpack the message */
		record = OTEL_FUNC_LOGS(log_record__unpack)
			(&batch->allocator, size, packed);

		if (record == NULL)
			batch->dropped++;
		else
		{
			if (batch->length >= batch->capacity)
				batch = otel_AddLogsBatch(exporter);

			batch->records[batch->length] = record;
			batch->length++;
			exporter->queueLength++;

			resourceLogs = llast(batch->resourceLogs);
			resourceLogs->scope_logs[0]->n_log_records++;
		}
	}
}

/*
 * Allocate a batch in its own top-level context and append it to exporter's queue.
 */
static struct otelLogsBatch *
otel_AddLogsBatch(struct otelLogsExporter *exporter)
{
	MemoryContext ctx = AllocSetContextCreate(NULL, /* parent */
											  PG_OTEL_LIBRARY " logs batch",
											  ALLOCSET_START_SMALL_SIZES);
	struct otelLogsBatch *batch = MemoryContextAllocZero(ctx, sizeof(*batch));

	batch->capacity = exporter->batchMax;
	batch->context = ctx;
	batch->records = MemoryContextAlloc(batch->context,
										sizeof(*(batch->records)) *
										batch->capacity);

	otel_InitProtobufCAllocator(&batch->allocator, batch->context);
	otel_AddLogsResource(batch, &exporter->resource);

	dlist_push_tail(&exporter->queue, &batch->list_node);

	return batch;
}

/*
 * Store a copy of resource in batch to be exported with any following records.
 */
static void
otel_AddLogsResource(struct otelLogsBatch *batch, struct otelResource *resource)
{
	MemoryContext oldContext = MemoryContextSwitchTo(batch->context);
	OTEL_TYPE_LOGS(ResourceLogs)  data;
	OTEL_TYPE_LOGS(ResourceLogs) *next;
	OTEL_TYPE_LOGS(ScopeLogs)     scopeLogsData;
	OTEL_TYPE_LOGS(ScopeLogs)    *scopeLogsList[1] = { &scopeLogsData };
	uint8_t *packed;
	size_t size;

	/* Prepare a ResourceLogs object containing a list of one ScopeLogs */
	OTEL_FUNC_LOGS(resource_logs__init)(&data);
	OTEL_FUNC_LOGS(scope_logs__init)(&scopeLogsData);
	data.resource = &resource->resource;
	data.scope_logs = scopeLogsList;
	data.n_scope_logs = 1;

	/* Copy it into the MemoryContext of the batch */
	packed = palloc(OTEL_FUNC_LOGS(resource_logs__get_packed_size)(&data));
	size = OTEL_FUNC_LOGS(resource_logs__pack)(&data, packed);
	next = OTEL_FUNC_LOGS(resource_logs__unpack)(&batch->allocator, size, packed);
	pfree(packed);

	/* Point that copy at the current position in the list of LogRecords */
	next->scope_logs[0]->log_records = batch->records + batch->length;
	next->scope_logs[0]->n_log_records = 0;

	batch->resourceLogs = lappend(batch->resourceLogs, next);

	MemoryContextSwitchTo(oldContext);
}

/*
 * Send request to the collector configured in exporter, ignoring any errors.
 */
static void
otel_SendLogsRequestToCollector(MemoryContext ctx, CURL *http,
								struct otelLogsExporter *exporter,
								OTEL_TYPE_EXPORT_LOGS(Request) *request)
{
	uint8_t *body = NULL;
	char     httpErrorBuffer[CURL_ERROR_SIZE];
	CURLcode result;
	size_t   size;

	struct curl_slist *headers = NULL;

	size = OTEL_FUNC_EXPORT_LOGS(request__get_packed_size)(request);
	body = MemoryContextAlloc(ctx, size);
	size = OTEL_FUNC_EXPORT_LOGS(request__pack)(request, body);

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
otel_SendLogsToCollector(struct otelLogsExporter *exporter, CURL *http)
{
	struct otelLogsBatch *batch;
	OTEL_TYPE_EXPORT_LOGS(Request) request;
	OTEL_TYPE_COMMON(InstrumentationScope) scopeData;
	ListCell *cell;

	if (dlist_is_empty(&exporter->queue))
		return;

	OTEL_FUNC_EXPORT_LOGS(request__init)(&request);
	OTEL_FUNC_COMMON(instrumentation_scope__init)(&scopeData);

	/*
	 * All log records come from the same instrumentation scope: this module.
	 * - https://opentelemetry.io/docs/specs/otel/glossary/#instrumentation-scope
	 */
	scopeData.name = PG_OTEL_LIBRARY;
	scopeData.version = PG_OTEL_VERSION;

	batch = dlist_head_element(struct otelLogsBatch, list_node,
							   &exporter->queue);

	request.n_resource_logs = batch->resourceLogs->length;
	request.resource_logs = MemoryContextAlloc(batch->context,
											   sizeof(*request.resource_logs) *
											   request.n_resource_logs);

	cell = list_head(batch->resourceLogs);
	for (int i = 0; i < request.n_resource_logs; i++)
	{
		request.resource_logs[i] = lfirst(cell);
		request.resource_logs[i]->schema_url = PG_OTEL_SCHEMA;
		request.resource_logs[i]->scope_logs[0]->schema_url = PG_OTEL_SCHEMA;
		request.resource_logs[i]->scope_logs[0]->scope = &scopeData;
#if PG_VERSION_NUM >= 130000
		cell = lnext(batch->resourceLogs, cell);
#else
		cell = lnext(cell);
#endif
	}

	otel_SendLogsRequestToCollector(batch->context, http, exporter, &request);

	dlist_pop_head_node(&exporter->queue);
	exporter->queueLength -= batch->length;

	MemoryContextDelete(batch->context);
}

static void
otel_InitLogsExporter(struct otelLogsExporter *exporter,
					  const struct otelConfiguration *config)
{
	dlist_init(&exporter->queue);
	exporter->endpoint = NULL;
	exporter->queueLength = 0;

	otel_InitResource(&exporter->resource);
	otel_LoadLogsConfig(exporter, config);
}

/*
 * Called by the background worker when PostgreSQL configuration changes.
 */
static void
otel_LoadLogsConfig(struct otelLogsExporter *exporter,
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

		Assert(config->otlpLogs.endpoint == NULL); /* TODO: per-signal */

		initStringInfo(&str);
		appendStringInfoString(&str, config->otlp.endpoint);

		if (!pg_str_endswith(config->otlp.endpoint, "/"))
			appendStringInfoString(&str, "/");

		appendStringInfoString(&str, "v1/logs");

		if (exporter->endpoint)
			pfree(exporter->endpoint);
		exporter->endpoint = str.data;
	}

	{
		Assert(config->otlpLogs.timeoutMS == 0); /* TODO: per-signal */

		exporter->timeoutMS = config->otlp.timeoutMS;
	}

	/*
	 * TODO: Accept settings for "Batch LogRecord Processor"
	 * - https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/
	 *
	 * TODO: Clamp both >= 1 for now.
	 */
	exporter->batchMax = 512;
	exporter->queueMax = 2048;

	exporter->insecure = false;
}
