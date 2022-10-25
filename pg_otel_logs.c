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
	 * - https://docs.opentelemetry.io/reference/specification/overview/
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
 * Called by the background worker to put a log message in the exporter batch.
 */
static void
otel_ReceiveLogMessage(struct otelLogsThread *t, const char *packed, size_t size)
{
	LWLockAcquire(t->lock, LW_EXCLUSIVE);

	if (t->batchHold.length < t->batchHold.capacity)
	{
		/* unpack returns NULL when it cannot unpack the message */
		OTEL_TYPE_LOGS(LogRecord) *record = OTEL_FUNC_LOGS(log_record__unpack)
			(&t->batchHold.allocator, size, (const uint8_t *)packed);

		if (record != NULL)
		{
			t->batchHold.records[t->batchHold.length] = record;
			t->batchHold.length++;
		}
		else
			t->batchHold.dropped++;
	}
	else
		t->batchHold.dropped++;

	LWLockRelease(t->lock);
}

static void
otel_InitLogsBatch(struct otelLogsBatch *batch, int capacity)
{
	Assert(batch != NULL);

	batch->capacity = capacity;
	batch->dropped = 0;
	batch->length = 0;
	batch->records = palloc0(sizeof(*(batch->records)) * capacity);
	batch->context = AllocSetContextCreate(NULL, /* parent */
										   PG_OTEL_LIBRARY " logs batch",
										   ALLOCSET_START_SMALL_SIZES);

	otel_InitProtobufCAllocator(&batch->allocator, batch->context);
}

/*
 * Called by the exporter thread after a batch has been sent to the collector.
 */
static void
otel_ResetLogsBatch(struct otelLogsBatch *batch)
{
	Assert(batch != NULL);

	batch->dropped = 0;
	batch->length = 0;
	MemoryContextReset(batch->context);
}

/*
 * Called by the exporter thread to send a batch to the collector.
 */
static void
otel_SendLogsToCollector(struct otelLogsThread *t,
						 const OTEL_TYPE_EXPORT_LOGS(Request) *request)
{
	char     httpErrorBuffer[CURL_ERROR_SIZE];
	CURLcode result;

	struct curl_slist *headers = NULL;
	uint8_t           *body;
	size_t             size;

	/* These do not error or their error can be ignored */
	curl_easy_reset(t->http);
	curl_easy_setopt(t->http, CURLOPT_ERRORBUFFER, httpErrorBuffer);
	curl_easy_setopt(t->http, CURLOPT_USERAGENT, PG_OTEL_USERAGENT);

	/* TODO: check errors */
	headers = curl_slist_append(headers, "Content-Type: application/x-protobuf");

	curl_easy_setopt(t->http, CURLOPT_HTTPHEADER, headers);

	/*
	 * TODO: gzip encoding
	 * TODO: retry and backoff
	 * - https://opentelemetry.io/docs/reference/specification/protocol/otlp/
	 * - https://opentelemetry.io/docs/reference/specification/protocol/exporter/
	 */

#ifdef PG_OTEL_DEBUG
	/* Print debugging information to stderr; off by default */
	curl_easy_setopt(t->http, CURLOPT_VERBOSE, 1);
#endif

	/* Read configuration and build the request body while holding the lock */
	{
		LWLockAcquire(t->lock, LW_EXCLUSIVE);

		body = MemoryContextAlloc
			(t->batchSend.context, OTEL_FUNC_EXPORT_LOGS(request__get_packed_size)(request));
		size = OTEL_FUNC_EXPORT_LOGS(request__pack)(request, body);

		result = curl_easy_setopt(t->http, CURLOPT_URL, t->endpoint);

		/* These do not error or their error can be ignored */
		curl_easy_setopt(t->http, CURLOPT_CONNECTTIMEOUT_MS, 1 + (t->timeoutMS / 2));
		curl_easy_setopt(t->http, CURLOPT_TIMEOUT_MS, t->timeoutMS);

		if (t->insecure)
		{
			curl_easy_setopt(t->http, CURLOPT_SSL_VERIFYHOST, 0);
			curl_easy_setopt(t->http, CURLOPT_SSL_VERIFYPEER, 0);
		}

		LWLockRelease(t->lock);
	}

	curl_easy_setopt(t->http, CURLOPT_POST, 1);
	curl_easy_setopt(t->http, CURLOPT_POSTFIELDS, body);
	curl_easy_setopt(t->http, CURLOPT_POSTFIELDSIZE, size);

	result = curl_easy_perform(t->http);

	/* TODO: check errors */
	result = result;

	curl_slist_free_all(headers);
	pfree(body);
}

/*
 * Exporter thread entry point. See [otel_StartLogsThread].
 */
static void *
otel_RunLogsThread(void *pointer)
{
	struct otelLogsThread *t = pointer;

	OTEL_TYPE_COMMON(InstrumentationScope) scopeData;
	OTEL_TYPE_LOGS(ResourceLogs)  resourceLogsData;
	OTEL_TYPE_LOGS(ResourceLogs) *resourceLogsList[1] = { &resourceLogsData };
	OTEL_TYPE_LOGS(ScopeLogs)     scopeLogsData;
	OTEL_TYPE_LOGS(ScopeLogs)    *scopeLogsList[1] = { &scopeLogsData };

	OTEL_FUNC_COMMON(instrumentation_scope__init)(&scopeData);
	OTEL_FUNC_LOGS(resource_logs__init)(&resourceLogsData);
	OTEL_FUNC_LOGS(scope_logs__init)(&scopeLogsData);

	/*
	 * All log records come from the same instrumentation scope: this module.
	 * The attributes are assigned according to v1.9.0 of the OpenTelemetry Specification.
	 * - https://docs.opentelemetry.io/reference/specification/glossary/#instrumentation-scope
	 * - https://docs.opentelemetry.io/reference/specification/schemas/overview/
	 */
	scopeData.name = PG_OTEL_LIBRARY;
	scopeData.version = PG_OTEL_VERSION;
	scopeLogsData.schema_url = "https://opentelemetry.io/schemas/1.9.0";
	scopeLogsData.scope = &scopeData;

	resourceLogsData.resource = &t->resource.resource;
	resourceLogsData.scope_logs = scopeLogsList;
	resourceLogsData.n_scope_logs = 1;

	for (;;)
	{
		/*
		 * Wait for the worker to indicate there are records to send.
		 */
		WaitLatch(&t->batchLatch, WL_LATCH_SET, -1, PG_WAIT_EXTENSION);
		LWLockAcquire(t->lock, LW_EXCLUSIVE);
		ResetLatch(&t->batchLatch);

		/*
		 * When there are records to send, flip the batches so the worker can
		 * continue to read from the local IPC while this thread sends to the
		 * remote collector.
		 */
		if (t->batchHold.length > 0)
		{
			struct otelLogsBatch temp = t->batchHold;
			t->batchHold = t->batchSend;
			t->batchSend = temp;
		}

		LWLockRelease(t->lock);

		/*
		 * When there are records to send, send them as a single request then
		 * release their resources.
		 */
		if (t->batchSend.length > 0)
		{
			OTEL_TYPE_EXPORT_LOGS(Request) request;
			OTEL_FUNC_EXPORT_LOGS(request__init)(&request);

			request.resource_logs       = resourceLogsList;
			request.n_resource_logs     = 1;
			scopeLogsData.log_records   = t->batchSend.records;
			scopeLogsData.n_log_records = t->batchSend.length;

			otel_SendLogsToCollector(t, &request);
			otel_ResetLogsBatch(&t->batchSend);
		}

		if (t->quit)
			break;
	}

	return NULL;
}

static void
otel_InitLogsThread(struct otelLogsThread *t, LWLock *lock, int batchCapacity)
{
	t->lock = lock;

	InitLatch(&t->batchLatch);
	otel_InitLogsBatch(&t->batchHold, batchCapacity);
	otel_InitLogsBatch(&t->batchSend, batchCapacity);
	otel_InitResource(&t->resource);

	t->http = curl_easy_init();
	if (t->http == NULL)
		ereport(FATAL, (errmsg("could not initialize curl for otel logs")));

	t->endpoint = NULL;
	t->insecure = false;
	t->timeoutMS = 0;
}

/*
 * Called by the background worker when PostgreSQL configuration changes.
 */
static void
otel_LoadLogsConfig(struct otelLogsThread *t, struct otelConfiguration *config)
{
	LWLockAcquire(t->lock, LW_EXCLUSIVE);

	otel_LoadResource(config, &t->resource);

	/*
	 * Per-signal URLs MUST be used as-is without any modification. When there
	 * is no path, append the root path.
	 *
	 * Without a per-signal configuration, the OTLP endpoint is a base URL and
	 * signals are sent relative to that.
	 *
	 * - https://opentelemetry.io/docs/reference/specification/protocol/exporter/
	 */
	{
		StringInfoData str;

		Assert(config->otlpLogs.endpoint == NULL); /* TODO: per-signal */

		initStringInfo(&str);
		appendStringInfoString(&str, config->otlp.endpoint);

		if (!pg_str_endswith(config->otlp.endpoint, "/"))
			appendStringInfoString(&str, "/");

		appendStringInfoString(&str, "v1/logs");

		if (t->endpoint)
			pfree(t->endpoint);
		t->endpoint = str.data;
	}

	{
		Assert(config->otlpLogs.timeoutMS == 0); /* TODO: per-signal */

		t->timeoutMS = config->otlp.timeoutMS;
	}

	LWLockRelease(t->lock);
}

/*
 * Called by the background worker to start the exporter thread.
 */
static void
otel_StartLogsThread(struct otelLogsThread *t)
{
	if (pthread_create(&t->thread, NULL, otel_RunLogsThread, (void *)t) != 0)
		ereport(FATAL, (errmsg("could not create otel logs thread")));
}

/*
 * Called by the background worker after it adds to the exporter batch.
 */
static void
otel_WakeLogsThread(struct otelLogsThread *t)
{
	LWLockAcquire(t->lock, LW_EXCLUSIVE);
	SetLatch(&t->batchLatch);
	LWLockRelease(t->lock);
}

/*
 * Called by the background worker after it sets t->quit.
 */
static void
otel_WaitLogsThread(struct otelLogsThread *t)
{
	pthread_join(t->thread, NULL);

	if (t->http != NULL)
		curl_easy_cleanup(t->http);
}
