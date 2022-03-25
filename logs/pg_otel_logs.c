/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include <stdio.h>
#include <sys/time.h>
#include <unistd.h>

#include "postgres.h"

#include "lib/stringinfo.h"
#include "libpq/libpq-be.h"
#include "miscadmin.h"
#include "fmgr.h"
#include "nodes/pg_list.h"
#include "postmaster/bgworker.h"
#include "postmaster/syslogger.h"
#include "storage/ipc.h"
#include "storage/latch.h"
#include "tcop/tcopprot.h"
#include "utils/elog.h"
#include "utils/guc.h"
#include "utils/memutils.h"

#if PG_VERSION_NUM >= 140000
#include "utils/wait_event.h"
#else
#include "pgstat.h"
#endif

#ifdef WIN32
#include "pthread-win32.h"
#else
#include <pthread.h>
#endif

#include "curl/curl.h"
#include "opentelemetry/proto/collector/logs/v1/logs_service.pb-c.h"

/* Dynamically loadable module */
PG_MODULE_MAGIC;

/* Hooks called when the module is (un)loaded */
void _PG_init(void);
void _PG_fini(void);

/* BackgroundWorker entry point */
void otel_WorkerMain(Datum main_arg) pg_attribute_noreturn();

/* Hooks overridden by this module */
static emit_log_hook_type next_EmitLogHook = NULL;

/* Worker signal handling */
static volatile sig_atomic_t otel_WorkerGotSIGHUP = false;
static volatile sig_atomic_t otel_WorkerGotSIGTERM = false;

/* Worker IPC */
#ifndef WIN32
static int otel_WorkerPipe[2] = {-1, -1};
#define OTEL_WORKER_PIPE_R otel_WorkerPipe[0]
#define OTEL_WORKER_PIPE_W otel_WorkerPipe[1]
#endif


/* Allocate size memory, as needed by ProtobufCAllocator.alloc */
static void *
otel_ProtobufAllocatorAlloc(void *context, size_t size) {
	return MemoryContextAlloc(context, size);
}

/* Free memory at pointer, as needed by ProtobufCAllocator.free */
static void
otel_ProtobufAllocatorFree(void *context, void *pointer) {
	pfree(pointer);
}

#define OTEL_FUNC_PROTO(name)     opentelemetry__proto__ ## name
#define OTEL_FUNC_COLLECTOR(name) OTEL_FUNC_PROTO(collector__logs__v1__ ## name)
#define OTEL_FUNC_COMMON(name)    OTEL_FUNC_PROTO(common__v1__ ## name)
#define OTEL_FUNC_LOGS(name)      OTEL_FUNC_PROTO(logs__v1__ ## name)

#define OTEL_TYPE_PROTO(name)     Opentelemetry__Proto__ ## name
#define OTEL_TYPE_COLLECTOR(name) OTEL_TYPE_PROTO(Collector__Logs__V1__ ## name)
#define OTEL_TYPE_COMMON(name)    OTEL_TYPE_PROTO(Common__V1__ ## name)
#define OTEL_TYPE_LOGS(name)      OTEL_TYPE_PROTO(Logs__V1__ ## name)
#define OTEL_TYPE_RESOURCE(name)  OTEL_TYPE_PROTO(Resource__V1__ ## name)


#define OTEL_RECORD_MAX_ATTRIBUTES 20

#define OTEL_SEVERITY_NUMBER(name) \
	OPENTELEMETRY__PROTO__LOGS__V1__SEVERITY_NUMBER__SEVERITY_NUMBER_ ## name

#define OTEL_VALUE_CASE(name) \
	OPENTELEMETRY__PROTO__COMMON__V1__ANY_VALUE__VALUE_ ## name ## _VALUE

struct otel_RecordBatch
{
	MemoryContext context;
	ProtobufCAllocator allocator;

	int length, capacity;
	OTEL_TYPE_LOGS(LogRecord) **records;
};

struct otel_ThreadState
{
	volatile sig_atomic_t quit;
	CURL *http;

	pthread_mutex_t              resourceMutex;
	OTEL_TYPE_RESOURCE(Resource) resource;

	pthread_cond_t          batchCondition;
	pthread_mutex_t         batchMutex;
	struct otel_RecordBatch batchHold, batchSend;
};

static void
otel_InitializeRecordBatch(struct otel_RecordBatch *batch, int capacity)
{
	Assert(batch != NULL);
	batch->capacity = capacity;
	batch->length = 0;
	batch->records = palloc0(sizeof(*(batch->records)) * capacity);
	batch->context = AllocSetContextCreate(NULL, "pg_otel_logs batch",
										   ALLOCSET_START_SMALL_SIZES);

	batch->allocator.allocator_data = batch->context;
	batch->allocator.alloc = otel_ProtobufAllocatorAlloc;
	batch->allocator.free = otel_ProtobufAllocatorFree;
}

static void
otel_ResetRecordBatch(struct otel_RecordBatch *batch)
{
	Assert(batch != NULL);
	batch->length = 0;
	MemoryContextReset(batch->context);
}


static void
otel_WorkerSendToCollector(MemoryContext ctx, CURL *http,
						   const OTEL_TYPE_COLLECTOR(ExportLogsServiceRequest) *request)
{
	char     httpErrorBuffer[CURL_ERROR_SIZE];
	CURLcode httpResult;

	struct curl_slist *requestHeaders = NULL;
	uint8_t           *requestBody;
	size_t             requestBodySize;

	requestBody = MemoryContextAlloc(ctx, OTEL_FUNC_COLLECTOR(export_logs_service_request__get_packed_size)(request));
	requestBodySize = OTEL_FUNC_COLLECTOR(export_logs_service_request__pack)(request, requestBody);

	// TODO consider gzip encoding?
	// TODO retry and backoff.
	// - https://opentelemetry.io/docs/reference/specification/protocol/otlp/
	// - https://opentelemetry.io/docs/reference/specification/protocol/exporter/

	// FIXME check errors
	curl_easy_reset(http);
	curl_easy_setopt(http, CURLOPT_ERRORBUFFER, httpErrorBuffer);
	curl_easy_setopt(http, CURLOPT_PROTOCOLS, CURLPROTO_HTTP | CURLPROTO_HTTPS);
	//curl_easy_setopt(http, CURLOPT_USERAGENT, PG_VERSION_STR);

	// Print debugging information to stderr; off by default.
	//curl_easy_setopt(http, CURLOPT_VERBOSE, 1);

	// Re-use connections; on by default.
	//curl_easy_setopt(http, CURLOPT_FORBID_REUSE, 0);

	// Check certificates; on by default.
	//curl_easy_setopt(http, CURLOPT_SSL_VERIFYHOST, 2);
	//curl_easy_setopt(http, CURLOPT_SSL_VERIFYPEER, 1);

	curl_easy_setopt(http, CURLOPT_CONNECTTIMEOUT_MS, 5000);
	curl_easy_setopt(http, CURLOPT_TIMEOUT_MS, 10000);

	//requestHeaders = curl_slist_append(requestHeaders, "Connection: keep-alive");
	requestHeaders = curl_slist_append(requestHeaders, "Content-Type: application/x-protobuf");

	curl_easy_setopt(http, CURLOPT_HTTPHEADER, requestHeaders);
	curl_easy_setopt(http, CURLOPT_URL, "http://localhost:4318/v1/logs");

	curl_easy_setopt(http, CURLOPT_POST, 1);
	curl_easy_setopt(http, CURLOPT_POSTFIELDS, requestBody);
	curl_easy_setopt(http, CURLOPT_POSTFIELDSIZE, requestBodySize);

	httpResult = curl_easy_perform(http);

	curl_slist_free_all(requestHeaders);
	pfree(requestBody);
}

static void
otel_WorkerProcessRecord(struct otel_ThreadState *state,
						 const char *packed, size_t size)
{
	pthread_mutex_lock(&(state->batchMutex));

	if (state->batchHold.length < state->batchHold.capacity)
	{
		state->batchHold.records[state->batchHold.length] =
			OTEL_FUNC_LOGS(log_record__unpack)(&(state->batchHold.allocator),
											   size, (const uint8_t *)packed);
		state->batchHold.length++;
	}
	else
	{
		// FIXME
	}

	pthread_mutex_unlock(&(state->batchMutex));
}


/*
 * Send r to the background worker in atomic chunks.
 */
static void
otel_SendToWorker(const OTEL_TYPE_LOGS(LogRecord) *r)
{
	PipeProtoChunk chunk;
	uint8_t *cursor;
	uint8_t *packed;
	size_t remaining;
	int rc;

	packed = palloc(OTEL_FUNC_LOGS(log_record__get_packed_size)(r));
	remaining = OTEL_FUNC_LOGS(log_record__pack)(r, packed);
	cursor = packed;

	chunk.proto.nuls[0] = chunk.proto.nuls[1] = '\0';
	chunk.proto.pid = MyProcPid;
#if PG_VERSION_NUM >= 150000
	chunk.proto.flags = 0;
#else
	chunk.proto.is_last = 'n';
#endif

	/* Write all but the last chunk */
	while (remaining > PIPE_MAX_PAYLOAD)
	{
		chunk.proto.len = PIPE_MAX_PAYLOAD;
		memcpy(chunk.proto.data, cursor, PIPE_MAX_PAYLOAD);
		rc = write(OTEL_WORKER_PIPE_W, &chunk, PIPE_HEADER_SIZE + PIPE_MAX_PAYLOAD);
		(void) rc;
		cursor += PIPE_MAX_PAYLOAD;
		remaining -= PIPE_MAX_PAYLOAD;
	}

	/* Write the last chunk */
#if PG_VERSION_NUM >= 150000
	chunk.proto.flags = PIPE_PROTO_IS_LAST;
#else
	chunk.proto.is_last = 'y';
#endif
	chunk.proto.len = remaining;
	memcpy(chunk.proto.data, cursor, remaining);
	rc = write(OTEL_WORKER_PIPE_W, &chunk, PIPE_HEADER_SIZE + remaining);
	(void) rc;

	pfree(packed);
}

/* Extract records from atomic chunks sent by backends */
static void
otel_WorkerProcessInput(struct otel_ThreadState *state,
						char *buffer, int *bufferOffset)
{
	struct Portion
	{
		int32 pid;
		StringInfoData data;
	};

	static List *portionBuckets[256];
	char *cursor = buffer;
	int remaining = *bufferOffset;


	while (remaining >= (int) (PIPE_HEADER_SIZE + 1))
	{
		int length;
		PipeProtoHeader header;
		List *portionBucket;
		ListCell *cell;
		struct Portion *empty = NULL;
		struct Portion *message = NULL;

		memcpy(&header, cursor, PIPE_HEADER_SIZE);
		if (header.nuls[0] != '\0' || header.nuls[1] != '\0' ||
			header.len <= 0 || header.len > PIPE_MAX_PAYLOAD ||
			header.pid == 0)
		{
			ereport(WARNING, (errmsg("unexpected message header")));

			/* Look for the start of a protocol header and try again */
			for (length = 1; length < remaining; length++)
			{
				if (cursor[length] == '\0')
					break;
			}
			remaining -= length;
			cursor += length;
			continue;
		}

		/* The length of the protocol chunk (header + data) */
		length = (PIPE_HEADER_SIZE + header.len);

		/* Give up when the buffer lacks the entire protocol chunk */
		if (remaining < length)
			break;

		/* Look for a partial message that matches the chunk PID */
		portionBucket = portionBuckets[header.pid % 256];
		foreach(cell, portionBucket)
		{
			struct Portion *slot = (struct Portion *) lfirst(cell);

			if (slot->pid == header.pid)
			{
				message = slot;
				break;
			}
			if (slot->pid == 0 && empty == NULL)
				empty = slot;
		}

#if PG_VERSION_NUM >= 150000
		if (message == NULL && (header.flags & PIPE_PROTO_IS_LAST))
#else
		if (message == NULL && header.is_last == 'y')
#endif
		{
			/* This chunk is a complete message; send it */
			otel_WorkerProcessRecord(state, cursor + PIPE_HEADER_SIZE, header.len);

			/* On to the next chunk */
			remaining -= length;
			cursor += length;
			continue;
		}

		/* This chunk is only part of a message */
		if (message == NULL)
		{
			if (empty == NULL)
			{
				/* Allocate and append space for a partial message */
				empty = palloc(sizeof(*empty));
				portionBucket = lappend(portionBucket, empty);
				portionBuckets[header.pid % 256] = portionBucket;
			}

			/* Start the partial message */
			message = empty;
			message->pid = header.pid;
			initStringInfo((StringInfo) &(message->data));
		}

		/* Append this chunk to its partial message */
		appendBinaryStringInfo((StringInfo) &(message->data),
							   cursor + PIPE_HEADER_SIZE, header.len);

#if PG_VERSION_NUM >= 150000
		if (header.flags & PIPE_PROTO_IS_LAST)
#else
		if (header.is_last == 'y')
#endif
		{
			/* The message is now complete; send it */
			otel_WorkerProcessRecord(state, message->data.data, message->data.len);

			/* Mark the slot unused and reclaim storage */
			message->pid = 0;
			pfree(message->data.data);
		}

		/* On to the next chunk */
		remaining -= length;
		cursor += length;
	}

	/* We don't have a full chunk, so left-align what remains in the buffer */
	if (remaining > 0 && cursor != buffer)
		memmove(buffer, cursor, remaining);
	*bufferOffset = remaining;

	/* Signal the thread that there may be records to send */
	pthread_mutex_lock(&(state->batchMutex));
	pthread_cond_signal(&(state->batchCondition));
	pthread_mutex_unlock(&(state->batchMutex));
}

static void *
otel_WorkerThread(void *pointer)
{
	struct otel_ThreadState *state = pointer;

	OTEL_TYPE_LOGS(ResourceLogs)  resourceLogsData;
	OTEL_TYPE_LOGS(ResourceLogs) *resourceLogsList[1] = { &resourceLogsData };
	OTEL_TYPE_LOGS(ScopeLogs)     scopeLogsData;
	OTEL_TYPE_LOGS(ScopeLogs)    *scopeLogsList[1] = { &scopeLogsData };

	OTEL_FUNC_LOGS(resource_logs__init)(&resourceLogsData);
	OTEL_FUNC_LOGS(scope_logs__init)(&scopeLogsData);

	// TODO: A real scope here.
	scopeLogsData.scope = NULL;

	// https://opentelemetry.io/docs/reference/specification/schemas/overview/
	scopeLogsData.schema_url = "https://opentelemetry.io/schemas/1.9.0";

	// TODO: A real resource here.
	resourceLogsData.resource = NULL;
	resourceLogsData.scope_logs = scopeLogsList;
	resourceLogsData.n_scope_logs = 1;

	for (;;)
	{
		/*
		 * Wait for the worker to indicate there are records to send.
		 */
		pthread_mutex_lock(&(state->batchMutex));
		pthread_cond_wait(&(state->batchCondition), &(state->batchMutex));

		/*
		 * When there are records to send, flip the batches so the worker can
		 * continue to read from the local pipe while this thread sends to the
		 * remote collector.
		 */
		if (state->batchHold.length > 0)
		{
			struct otel_RecordBatch temp = state->batchHold;
			state->batchHold = state->batchSend;
			state->batchSend = temp;
		}

		pthread_mutex_unlock(&(state->batchMutex));

		/*
		 * When there are records to send, send them as a single request then
		 * release their resources.
		 */
		if (state->batchSend.length > 0)
		{
			OTEL_TYPE_COLLECTOR(ExportLogsServiceRequest) request;
			OTEL_FUNC_COLLECTOR(export_logs_service_request__init)(&request);

			request.resource_logs       = resourceLogsList;
			request.n_resource_logs     = 1;
			scopeLogsData.log_records   = state->batchSend.records;
			scopeLogsData.n_log_records = state->batchSend.length;

			otel_WorkerSendToCollector(state->batchSend.context, state->http,
									   &request);

			otel_ResetRecordBatch(&(state->batchSend));
		}

		if (state->quit)
			break;
	}

	return NULL;
}

static void
otel_WorkerHandleSIGHUP(SIGNAL_ARGS)
{
	int save_errno = errno;
	otel_WorkerGotSIGHUP = true;
	SetLatch(MyLatch);
	errno = save_errno;
}

static void
otel_WorkerHandleSIGTERM(SIGNAL_ARGS)
{
	int save_errno = errno;
	otel_WorkerGotSIGTERM = true;
	SetLatch(MyLatch);
	errno = save_errno;
}

void
otel_WorkerMain(Datum main_arg)
{
	char bufferData[2 * PIPE_CHUNK_SIZE];
	int bufferOffset = 0;
	struct otel_ThreadState state = {};
	pthread_t thread;
	WaitEventSet *wes;

	/* Register our signal handlers */
	pqsignal(SIGHUP, otel_WorkerHandleSIGHUP);
	pqsignal(SIGTERM, otel_WorkerHandleSIGTERM);

	/* Ready to receive signals */
	BackgroundWorkerUnblockSignals();

	/* Close our copy of the write end of the pipe */
#ifndef WIN32
	if (OTEL_WORKER_PIPE_W >= 0)
		close(OTEL_WORKER_PIPE_W);
	OTEL_WORKER_PIPE_W = -1;
#endif

	/* Initialize CURL */
	curl_global_init(CURL_GLOBAL_ALL);
	state.http = curl_easy_init();
	if (state.http == NULL)
		ereport(FATAL, (errmsg("could not initialize CURL for otel logs")));

	if (pthread_cond_init(&state.batchCondition, NULL) != 0 ||
		pthread_mutex_init(&state.batchMutex, NULL) != 0 ||
		pthread_mutex_init(&state.resourceMutex, NULL) != 0)
		ereport(FATAL, (errmsg("could not initialize otel thread synchronization")));

	/* Initialize the batches that hold records waiting to send */
	otel_InitializeRecordBatch(&state.batchHold, 500);
	otel_InitializeRecordBatch(&state.batchSend, 500);

	if (pthread_create(&thread, NULL, otel_WorkerThread, (void *)&state) != 0)
		ereport(FATAL, (errmsg("could not create otel thread")));

	/* Set up a WaitEventSet for our latch and pipe */
	wes = CreateWaitEventSet(CurrentMemoryContext, 3);
	AddWaitEventToSet(wes, WL_LATCH_SET, PGINVALID_SOCKET, MyLatch, NULL);
	AddWaitEventToSet(wes, WL_POSTMASTER_DEATH, PGINVALID_SOCKET, NULL, NULL);
#ifndef WIN32
	AddWaitEventToSet(wes, WL_SOCKET_READABLE, OTEL_WORKER_PIPE_R, NULL, NULL);
#endif

	for (;;)
	{
		WaitEvent event;

		/* Wait forever for some work */
		WaitEventSetWait(wes, -1, &event, 1, PG_WAIT_EXTENSION);
		ResetLatch(MyLatch);

		if (otel_WorkerGotSIGTERM)
			break;

		if (otel_WorkerGotSIGHUP)
		{
			// FIXME: ProcessConfigFile(PGC_SIGHUP);
			otel_WorkerGotSIGHUP = false;
		}

		if (event.events == WL_SOCKET_READABLE)
		{
			int n = read(OTEL_WORKER_PIPE_R,
						 bufferData + bufferOffset,
						 sizeof(bufferData) - bufferOffset);

			if (n < 0)
			{
				if (errno != EINTR)
					ereport(LOG,
							(errcode_for_socket_access(),
							 errmsg("could not read from otel pipe: %m")));
			}
			else if (n > 0)
			{
				bufferOffset += n;
				otel_WorkerProcessInput(&state, bufferData, &bufferOffset);
			}
			else
			{
				/* FIXME */
				ereport(LOG, (errmsg("otel pipe EOF")));
			}
		}
	}

	/* TODO: flush */
	// Look for techniques around ShutdownXLOG.

	/* Signal the thread and wait for it to finish sending records */
	state.quit = true;
	pthread_mutex_lock(&(state.batchMutex));
	pthread_cond_signal(&(state.batchCondition));
	pthread_mutex_unlock(&(state.batchMutex));
	pthread_join(thread, NULL);

	if (state.http != NULL)
		curl_easy_cleanup(state.http);
	curl_global_cleanup();

	/* Exit zero so we aren't restarted */
	proc_exit(0);
}

/*
 * Append key and value to record->attributes by storing them in key_values and any_values.
 */
static void
otel_AttributeInt(OTEL_TYPE_LOGS(LogRecord) *record,
				  OTEL_TYPE_COMMON(KeyValue) *keyValues,
				  OTEL_TYPE_COMMON(AnyValue) *anyValues,
				  const char *key, int value)
{
	size_t i = record->n_attributes;
	Assert(i < OTEL_RECORD_MAX_ATTRIBUTES);

	OTEL_FUNC_COMMON(key_value__init)(&keyValues[i]);
	OTEL_FUNC_COMMON(any_value__init)(&anyValues[i]);

	record->attributes[i] = &keyValues[i];
	keyValues[i].key = (char *)key;
	keyValues[i].value = &anyValues[i];
	anyValues[i].value_case = OTEL_VALUE_CASE(INT);
	anyValues[i].int_value = value;

	++record->n_attributes;
}

/*
 * Append key and value to record->attributes by storing them in key_values and any_values.
 */
static void
otel_AttributeStr(OTEL_TYPE_LOGS(LogRecord) *record,
				  OTEL_TYPE_COMMON(KeyValue) *keyValues,
				  OTEL_TYPE_COMMON(AnyValue) *anyValues,
				  const char *key, const char *value)
{
	size_t i = record->n_attributes;
	Assert(i < OTEL_RECORD_MAX_ATTRIBUTES);

	OTEL_FUNC_COMMON(key_value__init)(&keyValues[i]);
	OTEL_FUNC_COMMON(any_value__init)(&anyValues[i]);

	record->attributes[i] = &keyValues[i];
	keyValues[i].key = (char *)key;
	keyValues[i].value = &anyValues[i];
	anyValues[i].value_case = OTEL_VALUE_CASE(STRING);
	anyValues[i].string_value = (char *)value;

	++record->n_attributes;
}

/*
 * Called when a log message is not supressed by log_min_messages.
 */
static void
otel_EmitLogHook(ErrorData *edata)
{
	OTEL_TYPE_COMMON(KeyValue)  attrKeyValues[OTEL_RECORD_MAX_ATTRIBUTES];
	OTEL_TYPE_COMMON(AnyValue)  attrAnyValues[OTEL_RECORD_MAX_ATTRIBUTES];
	OTEL_TYPE_COMMON(KeyValue) *attrList[OTEL_RECORD_MAX_ATTRIBUTES];
	OTEL_TYPE_COMMON(AnyValue)  body;
	OTEL_TYPE_LOGS(LogRecord)   record;
	struct timeval tv;
	uint64_t unixNanoSec;

	gettimeofday(&tv, NULL);
	unixNanoSec = tv.tv_sec * 1000000000 + tv.tv_usec * 1000;

	OTEL_FUNC_LOGS(log_record__init)(&record);
	OTEL_FUNC_COMMON(any_value__init)(&body);

	record.attributes = attrList;
	record.body = &body;
	record.body->value_case = OTEL_VALUE_CASE(STRING);
	record.body->string_value = edata->message;
	record.observed_time_unix_nano = unixNanoSec;
	record.time_unix_nano = unixNanoSec;

	/*
	 * Set severity number and text according to OpenTelemetry Log Data Model
	 * and error_severity() in elog.c.
	 * - https://opentelemetry.io/docs/reference/specification/logs/data-model/
	 *
	 * > ["SeverityText"] is the original string representation of the severity
	 * > as it is known at the source.
	 *
	 * > If the log record represents a non-erroneous event the "SeverityNumber"
	 * > field … may be set to any numeric value less than ERROR (numeric 17).
	 *
	 * > If "SeverityNumber" is present and has a value of ERROR (numeric 17)
	 * > or higher then it is an indication that the log record represents an
	 * > erroneous situation.
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
			record.severity_number = OTEL_SEVERITY_NUMBER(TRACE);
			record.severity_text = "DEBUG";
			break;
		case DEBUG4:
			record.severity_number = OTEL_SEVERITY_NUMBER(TRACE2);
			record.severity_text = "DEBUG";
			break;
		case DEBUG3:
			record.severity_number = OTEL_SEVERITY_NUMBER(TRACE3);
			record.severity_text = "DEBUG";
			break;
		case DEBUG2:
			record.severity_number = OTEL_SEVERITY_NUMBER(TRACE4);
			record.severity_text = "DEBUG";
			break;
		case DEBUG1:
			record.severity_number = OTEL_SEVERITY_NUMBER(DEBUG);
			record.severity_text = "DEBUG";
			break;
		case LOG:
		case LOG_SERVER_ONLY:
			record.severity_number = OTEL_SEVERITY_NUMBER(INFO);
			record.severity_text = "LOG";
			break;
		case INFO:
			record.severity_number = OTEL_SEVERITY_NUMBER(INFO);
			record.severity_text = "INFO";
			break;
		case NOTICE:
			record.severity_number = OTEL_SEVERITY_NUMBER(INFO2);
			record.severity_text = "NOTICE";
			break;
		case WARNING:
		case WARNING_CLIENT_ONLY:
			record.severity_number = OTEL_SEVERITY_NUMBER(WARN);
			record.severity_text = "WARNING";
			break;
		case ERROR:
			record.severity_number = OTEL_SEVERITY_NUMBER(ERROR);
			record.severity_text = "ERROR";
			break;
		case FATAL:
			record.severity_number = OTEL_SEVERITY_NUMBER(FATAL);
			record.severity_text = "FATAL";
			break;
		case PANIC:
			record.severity_number = OTEL_SEVERITY_NUMBER(FATAL2);
			record.severity_text = "PANIC";
			break;
		default:
			record.severity_number = OTEL_SEVERITY_NUMBER(FATAL2);
	}

	/*
	 * Set attributes according to OpenTelemetry Semantic Conventions.
	 * - https://opentelemetry.io/docs/reference/specification/overview/
	 */

	if (MyProcPid != 0)
		otel_AttributeInt(&record, attrKeyValues, attrAnyValues,
						  "process.pid", MyProcPid);

	if (edata->funcname != NULL)
		otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
						  "code.function", edata->funcname);

	if (edata->filename != NULL)
	{
		otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
						  "code.filepath", edata->filename);
		otel_AttributeInt(&record, attrKeyValues, attrAnyValues,
						  "code.lineno", edata->lineno);
	}

	if (MyProcPort != NULL)
	{
		if (MyProcPort->database_name != NULL)
			otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
							  "db.name", MyProcPort->database_name);

		if (MyProcPort->user_name != NULL)
			otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
							  "db.user", MyProcPort->user_name);

		// TODO: MyProcPort->remote_host + MyProcPort->remote_port
	}

	if (debug_query_string != NULL && !edata->hide_stmt)
	{
		otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
						  "db.statement", debug_query_string);

		if (edata->cursorpos > 0)
			otel_AttributeInt(&record, attrKeyValues, attrAnyValues,
							  "db.postgresql.cursor_position",
							  edata->cursorpos);
	}

	if (edata->internalquery != NULL)
	{
		otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
						  "db.postgresql.internal_query",
						  edata->internalquery);

		if (edata->internalpos > 0)
			otel_AttributeInt(&record, attrKeyValues, attrAnyValues,
							  "db.postgresql.internal_position",
							  edata->internalpos);
	}

	if (edata->context != NULL && !edata->hide_ctx)
		otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
						  "db.postgresql.context", edata->context);

	if (edata->sqlerrcode != 0)
		otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
						  "db.postgresql.state_code",
						  unpack_sql_state(edata->sqlerrcode));

	if (edata->hint != NULL)
		otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
						  "db.postgresql.hint", edata->hint);

	if (edata->detail_log != NULL)
		otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
						  "db.postgresql.detail", edata->detail_log);

	else if (edata->detail != NULL)
		otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
						  "db.postgresql.detail", edata->detail);

	if (application_name != NULL && application_name[0] != '\0')
		otel_AttributeStr(&record, attrKeyValues, attrAnyValues,
						  "db.postgresql.application_name", application_name);

	// TODO: backend_type
	// TODO: session_id
	// TODO: vxid + txid
	// TODO: leader_pid
	// TODO: query_id

	otel_SendToWorker(&record);

	/* Call the next log processor */
	if (next_EmitLogHook)
		(*next_EmitLogHook) (edata);
}


/* Called when the module is loaded */
void
_PG_init(void)
{
	BackgroundWorker worker;

#ifndef WIN32
	if (pipe(otel_WorkerPipe) < 0)
		ereport(FATAL,
				(errcode_for_socket_access(),
				 errmsg("could not create pipe for worker: %m")));
#endif

	/*
	 * Register our background worker to start immediately. Restart it without
	 * delay if it crashes.
	 */
	MemSet(&worker, 0, sizeof(BackgroundWorker));
	worker.bgw_flags = BGWORKER_SHMEM_ACCESS;
	worker.bgw_start_time = BgWorkerStart_PostmasterStart;
	snprintf(worker.bgw_name, BGW_MAXLEN, "OpenTelemetry logs exporter");
	snprintf(worker.bgw_library_name, BGW_MAXLEN, "pg_otel_logs");
	snprintf(worker.bgw_function_name, BGW_MAXLEN, "otel_WorkerMain");
	RegisterBackgroundWorker(&worker);

	/* Install our log processor */
	next_EmitLogHook = emit_log_hook;
	emit_log_hook = otel_EmitLogHook;
}

/*
 * Called when the module is unloaded, which is never.
 * - https://git.postgresql.org/gitweb/?p=postgresql.git;f=src/backend/utils/fmgr/dfmgr.c;hb=REL_10_0#l389
 */
void
_PG_fini(void)
{
	/* Uninstall our log processor */
	emit_log_hook = next_EmitLogHook;
}
