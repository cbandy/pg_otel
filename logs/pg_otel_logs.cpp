/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

//#include "opentelemetry/exporters/otlp/protobuf_include_prefix.h"
#include "opentelemetry/proto/logs/v1/logs.pb.h"
#include "opentelemetry/proto/resource/v1/resource.pb.h"
#include "opentelemetry/proto/collector/logs/v1/logs_service.pb.h"
//#include "opentelemetry/exporters/otlp/protobuf_include_suffix.h"

extern "C"
{
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

#if PG_VERSION_NUM >= 140000
#include "utils/wait_event.h"
#else
#include "pgstat.h"
#endif

#include "curl/curl.h"

/* Dynamically loadable module */
PG_MODULE_MAGIC;

/* Hooks called when the module is (un)loaded */
void _PG_init(void);
void _PG_fini(void);

/* BackgroundWorker entry point */
void otel_WorkerMain(Datum main_arg) pg_attribute_noreturn();
}

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

namespace proto_collector = opentelemetry::proto::collector::logs::v1;
namespace proto_logs = opentelemetry::proto::logs::v1;


static void otel_WorkerProcessRecord(proto_logs::LogRecord *, CURL *);

// TODO consider using C packing.
/* Send r to the background worker in atomic chunks */
static void
otel_SendToWorker(proto_logs::LogRecord *r)
{
	std::string s;
	r->SerializeToString(&s);

	PipeProtoChunk chunk;
	char *cursor = (char *)s.data();
	int remaining = s.size();
	int rc;

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
}

// FIXME: remove
static FILE *debug;

/* Extract records from atomic chunks sent by backends */
static void
otel_WorkerProcessInput(char *buffer, int *bufferOffset, CURL *http)
{
	typedef struct {
		int32 pid;
		StringInfoData data;
	} partialMessage;

	static List *partialMessageBuckets[256];
	char *cursor = buffer;
	int remaining = *bufferOffset;


	while (remaining >= (int) (PIPE_HEADER_SIZE + 1))
	{
		int length;
		PipeProtoHeader header;
		List *partialMessageBucket;
		ListCell *cell;
		partialMessage *empty = NULL;
		partialMessage *message = NULL;

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
		partialMessageBucket = partialMessageBuckets[header.pid % 256];
		foreach(cell, partialMessageBucket)
		{
			partialMessage *slot = (partialMessage *) lfirst(cell);

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
			/* TODO: send it from buffer (cursor + PIPE_HEADER_SIZE, header.len) */
			//fwrite(cursor + PIPE_HEADER_SIZE, 1, header.len, debug);

			proto_logs::LogRecord record;
			record.ParseFromString(std::string(cursor + PIPE_HEADER_SIZE, header.len));

			std::string text = record.DebugString();
			fwrite(text.data(), 1, text.size(), debug);

			otel_WorkerProcessRecord(&record, http);

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
				empty = (partialMessage *)palloc(sizeof(partialMessage));
				partialMessageBucket = lappend(partialMessageBucket, empty);
				partialMessageBuckets[header.pid % 256] = partialMessageBucket;
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
			/* TODO: send it from slot (s->data, s->len) */
			fwrite(message->data.data, 1, message->data.len, debug);

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
}

static void
otel_WorkerProcessRecord(proto_logs::LogRecord *r, CURL *http)
{
	proto_collector::ExportLogsServiceRequest request;
	proto_collector::ExportLogsServiceResponse response;

	auto resource_logs = request.add_resource_logs();
	auto instrumentation_logs = resource_logs->add_instrumentation_library_logs();

	// TODO resource_logs->mutable_resource();
	// TODO resource_logs->set_schema_url();

	// TODO instrumentation_logs->mutable_instrumentation_library();
	// TODO instrumentation_logs->set_schema_url();

	*instrumentation_logs->add_log_records() = *r;

	//grpc::ClientContext context;
	// TODO context.set_deadline();
	// TODO context.AddMetadata();


	char http_error_buffer[CURL_ERROR_SIZE];
	CURLcode http_result;
	struct curl_slist *request_headers = NULL;

	std::string request_body = request.SerializeAsString();
	// request.SerializeToArray(void * data, int size)

	// TODO consider gzip encoding?
	// TODO retry and backoff.
	// - https://opentelemetry.io/docs/reference/specification/protocol/otlp/
	// - https://opentelemetry.io/docs/reference/specification/protocol/exporter/

	// FIXME check errors
	curl_easy_reset(http);
	curl_easy_setopt(http, CURLOPT_ERRORBUFFER, http_error_buffer);
	curl_easy_setopt(http, CURLOPT_PROTOCOLS, CURLPROTO_HTTP | CURLPROTO_HTTPS);
	//curl_easy_setopt(http, CURLOPT_USERAGENT, PG_VERSION_STR);

	// Print debugging information to stderr; off by default.
	curl_easy_setopt(http, CURLOPT_VERBOSE, 1);

	// Re-use connections; on by default.
	//curl_easy_setopt(http, CURLOPT_FORBID_REUSE, 0);

	// Check certificates; on by default.
	//curl_easy_setopt(http, CURLOPT_SSL_VERIFYHOST, 2);
	//curl_easy_setopt(http, CURLOPT_SSL_VERIFYPEER, 1);

	curl_easy_setopt(http, CURLOPT_CONNECTTIMEOUT_MS, 5000);
	curl_easy_setopt(http, CURLOPT_TIMEOUT_MS, 10000);

	//request_headers = curl_slist_append(request_headers, "Connection: keep-alive");
	request_headers = curl_slist_append(request_headers, "Content-Type: application/x-protobuf");

	curl_easy_setopt(http, CURLOPT_HTTPHEADER, request_headers);
	curl_easy_setopt(http, CURLOPT_URL, "http://localhost:4318/v1/logs");

	curl_easy_setopt(http, CURLOPT_POST, 1);
	curl_easy_setopt(http, CURLOPT_POSTFIELDS, request_body.data());
	curl_easy_setopt(http, CURLOPT_POSTFIELDSIZE, request_body.size());

	http_result = curl_easy_perform(http);

	curl_slist_free_all(request_headers);
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

/* Called as the background worker */
void
otel_WorkerMain(Datum main_arg)
{
	char bufferData[2 * PIPE_CHUNK_SIZE];
	int bufferOffset = 0;
	CURL *http;
	WaitEventSet *wes;

	/* Register our signal handlers */
	pqsignal(SIGHUP, otel_WorkerHandleSIGHUP);
	pqsignal(SIGTERM, otel_WorkerHandleSIGTERM);

	/* Ready to receive signals */
	BackgroundWorkerUnblockSignals();

	/* Initialize CURL */
	curl_global_init(CURL_GLOBAL_ALL);
	http = curl_easy_init();
	if (http == NULL)
		ereport(FATAL, (errmsg("could not initialize CURL")));

	debug = fopen("/tmp/pg-otel.txt", "w");
	if (debug)
		setvbuf(debug, NULL, PG_IOLBF, 0);
	else
		ereport(FATAL,
				(errcode_for_file_access(),
				 errmsg("could not open file \"/tmp/pg-otel.txt\": %m")));

	/* Close our copy of the write end of the pipe */
#ifndef WIN32
	if (OTEL_WORKER_PIPE_W >= 0)
		close(OTEL_WORKER_PIPE_W);
	OTEL_WORKER_PIPE_W = -1;
#endif

	// TODO add batch timeout below.

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
				otel_WorkerProcessInput(bufferData, &bufferOffset, http);
			}
			else
			{
				/* FIXME */
				ereport(LOG, (errmsg("otel pipe EOF")));
			}
		}
	}

	/* TODO: flush */

	fclose(debug);

	if (http)
		curl_easy_cleanup(http);
	curl_global_cleanup();

	/* Exit zero so we aren't restarted */
	proc_exit(0);
}

static void
otel_AttributeInt(proto_logs::LogRecord *record, const char *key, int value)
{
	opentelemetry::proto::common::v1::KeyValue *attribute = record->add_attributes();
	attribute->set_key(key);
	attribute->mutable_value()->set_int_value(value);
}

static void
otel_AttributeStr(proto_logs::LogRecord *record, const char *key, const char *value)
{
	if (value == NULL)
		return;

	opentelemetry::proto::common::v1::KeyValue *attribute = record->add_attributes();
	attribute->set_key(key);
	attribute->mutable_value()->set_string_value(value);
}

/*
 * Called when a log message is not supressed by log_min_messages.
 */
static void
otel_EmitLogHook(ErrorData *edata)
{
	proto_logs::LogRecord record;
	struct timeval tv;
	uint64 unix_nano;

	gettimeofday(&tv, NULL);
	unix_nano = tv.tv_sec * 1000000000 + tv.tv_usec * 1000;

	record.set_observed_time_unix_nano(unix_nano);
	record.set_time_unix_nano(unix_nano);

	record.mutable_body()->set_string_value(edata->message);

	/*
	 * Set severity number and text according to OpenTelemetry Log Data Model
	 * and error_severity() in elog.c.
	 * - https://opentelemetry.io/docs/reference/specification/logs/data-model/
	 *
	 * > If the log record represents a non-erroneous event the "SeverityNumber"
	 * > field … may be set to any numeric value less than ERROR (numeric 17).
	 *
	 * > If "SeverityNumber" is present and has a value of ERROR (numeric 17)
	 * > or higher then it is an indication that the log record represents an
	 * > erroneous situation.
	 */
	switch (edata->elevel)
	{
		case DEBUG1:
			/* elog.c: syslog_level = LOG_DEBUG */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_DEBUG);
			record.set_severity_text("DEBUG");
			break;
		case DEBUG2:
			/* elog.c: syslog_level = LOG_DEBUG */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_TRACE4);
			record.set_severity_text("DEBUG");
			break;
		case DEBUG3:
			/* elog.c: syslog_level = LOG_DEBUG */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_TRACE3);
			record.set_severity_text("DEBUG");
			break;
		case DEBUG4:
			/* elog.c: syslog_level = LOG_DEBUG */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_TRACE2);
			record.set_severity_text("DEBUG");
			break;
		case DEBUG5:
			/* elog.c: syslog_level = LOG_DEBUG */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_TRACE);
			record.set_severity_text("DEBUG");
			break;
		case LOG:
		case LOG_SERVER_ONLY:
			/* elog.c: syslog_level = LOG_INFO */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_INFO);
			record.set_severity_text("LOG");
			break;
		case INFO:
			/* elog.c: syslog_level = LOG_INFO */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_INFO);
			record.set_severity_text("INFO");
			break;
		case NOTICE:
			/* elog.c: syslog_level = LOG_NOTICE */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_INFO2);
			record.set_severity_text("NOTICE");
			break;
		case WARNING:
		case WARNING_CLIENT_ONLY:
			/* elog.c: syslog_level = LOG_NOTICE */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_WARN);
			record.set_severity_text("WARNING");
			break;
		case ERROR:
			/* elog.c: syslog_level = LOG_WARNING */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_ERROR);
			record.set_severity_text("ERROR");
			break;
		case FATAL:
			/* elog.c: syslog_level = LOG_ERR */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_FATAL);
			record.set_severity_text("FATAL");
			break;
		case PANIC:
			/* elog.c: syslog_level = LOG_CRIT */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_FATAL2);
			record.set_severity_text("PANIC");
			break;
		default:
			/* elog.c: syslog_level = LOG_CRIT */
			record.set_severity_number(proto_logs::SEVERITY_NUMBER_FATAL2);
	}

	/*
	 * https://opentelemetry.io/docs/reference/specification/overview/#semantic-conventions
	 */

	// A string containing the time when the data was accessed in the ISO 8601 format expressed in UTC. (dTtZ)

	// TODO: Move this to the worker and into Resource, or omit it entirely.
	//otel_AttributeStr(&record, "db.system", "postgresql");

	if (MyProcPid != 0)
		otel_AttributeInt(&record, "process.pid", MyProcPid);

	otel_AttributeStr(&record, "code.function", edata->funcname);

	if (edata->filename)
	{
		otel_AttributeStr(&record, "code.filepath", edata->filename);
		otel_AttributeInt(&record, "code.lineno", edata->lineno);
	}

	if (MyProcPort)
	{
		otel_AttributeStr(&record, "db.name", MyProcPort->database_name);
		otel_AttributeStr(&record, "db.user", MyProcPort->user_name);

		// TODO: MyProcPort->remote_host + MyProcPort->remote_port
	}

	if (debug_query_string != NULL && !edata->hide_stmt)
	{
		otel_AttributeStr(&record, "db.statement", debug_query_string);

		if (edata->cursorpos > 0)
			otel_AttributeInt(&record, "db.postgresql.cursor_position",
							  edata->cursorpos);
	}

	if (edata->internalquery)
	{
		otel_AttributeStr(&record, "db.postgresql.internal_query",
						  edata->internalquery);

		if (edata->internalpos > 0)
			otel_AttributeInt(&record, "db.postgresql.internal_position",
							  edata->internalpos);
	}

	if (edata->context && !edata->hide_ctx)
		otel_AttributeStr(&record, "db.postgresql.context", edata->context);

	if (edata->sqlerrcode)
		otel_AttributeStr(&record, "db.postgresql.state_code",
						  unpack_sql_state(edata->sqlerrcode));

	otel_AttributeStr(&record, "db.postgresql.hint", edata->hint);
	otel_AttributeStr(&record, "db.postgresql.detail",
					  (edata->detail_log) ? edata->detail_log : edata->detail);

	if (application_name && application_name[0] != '\0')
		otel_AttributeStr(&record, "db.postgresql.application_name",
						  application_name);

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

	// TODO: arena?

	/* Install our log processor */
	next_EmitLogHook = emit_log_hook;
	emit_log_hook = otel_EmitLogHook;
}

/* Called when the module is unloaded */
void
_PG_fini(void)
{
	/* Uninstall our log processor */
	emit_log_hook = next_EmitLogHook;
}
