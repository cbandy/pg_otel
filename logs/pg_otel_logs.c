/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include <stdio.h>
#include <sys/time.h>
#include <unistd.h>

#include "postgres.h"
#include "miscadmin.h"
#include "fmgr.h"
#include "lib/stringinfo.h"
#include "nodes/pg_list.h"
#include "postmaster/bgworker.h"
#include "postmaster/syslogger.h"
#include "storage/ipc.h"
#include "storage/latch.h"
#include "utils/elog.h"
#include "utils/json.h"
#include "utils/wait_event.h"

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

/* Send all of s to the background worker in atomic chunks */
static void
otel_SendToWorker(StringInfoData *s)
{
	PipeProtoChunk chunk;
	char *cursor = s->data;
	int remaining = s->len;
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

static FILE *debug;

/* Extract messages from atomic chunks sent by backends */
static void
otel_WorkerProcessInput(char *buffer, int *bufferOffset)
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
			fwrite(cursor + PIPE_HEADER_SIZE, 1, header.len, debug);

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
				empty = palloc(sizeof(partialMessage));
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
	WaitEventSet *wes;

	/* Register our signal handlers */
	pqsignal(SIGHUP, otel_WorkerHandleSIGHUP);
	pqsignal(SIGTERM, otel_WorkerHandleSIGTERM);

	/* Ready to receive signals */
	BackgroundWorkerUnblockSignals();

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
				otel_WorkerProcessInput(bufferData, &bufferOffset);
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

	/* Exit zero so we aren't restarted */
	proc_exit(0);
}


/* Convert elevel to … according to OpenTelemetry Log Data Model */
static int
otel_SeverityNumber(int elevel)
{
	/*
	 * > If the log record represents a non-erroneous event the "SeverityNumber"
	 * > field … may be set to any numeric value less than ERROR (numeric 17).
	 *
	 * > If "SeverityNumber" is present and has a value of ERROR (numeric 17)
	 * > or higher then it is an indication that the log record represents an
	 * > erroneous situation.
	 */
	switch (elevel)
	{
		case DEBUG1:
			/* elog.c: syslog_level = LOG_DEBUG */
			return 5; /* DEBUG */
		case DEBUG2:
			/* elog.c: syslog_level = LOG_DEBUG */
			return 4; /* TRACE4 */
		case DEBUG3:
			/* elog.c: syslog_level = LOG_DEBUG */
			return 3; /* TRACE3 */
		case DEBUG4:
			/* elog.c: syslog_level = LOG_DEBUG */
			return 2; /* TRACE2 */
		case DEBUG5:
			/* elog.c: syslog_level = LOG_DEBUG */
			return 1; /* TRACE1 */
		case LOG:
		case LOG_SERVER_ONLY:
		case INFO:
			/* elog.c: syslog_level = LOG_INFO */
			return 9; /* INFO */
		case NOTICE:
			/* elog.c: syslog_level = LOG_NOTICE */
			return 10; /* INFO2 */
		case WARNING:
		case WARNING_CLIENT_ONLY:
			/* elog.c: syslog_level = LOG_NOTICE */
			return 13; /* WARN */
		case ERROR:
			/* elog.c: syslog_level = LOG_WARNING */
			return 17; /* ERROR */
		case FATAL:
			/* elog.c: syslog_level = LOG_ERR */
			return 21; /* FATAL */
		case PANIC:
		default:
			/* elog.c: syslog_level = LOG_CRIT */
			return 22; /* FATAL2 */
	}
}

/* Convert elevel to a JSON string identical to error_severity() in elog.c */
static const char *
otel_SeverityText(int elevel)
{
	const char *text;

	switch (elevel)
	{
		case DEBUG1:
		case DEBUG2:
		case DEBUG3:
		case DEBUG4:
		case DEBUG5:
			text = "\"DEBUG\"";
			break;
		case LOG:
		case LOG_SERVER_ONLY:
			text = "\"LOG\"";
			break;
		case INFO:
			text = "\"INFO\"";
			break;
		case NOTICE:
			text = "\"NOTICE\"";
			break;
		case WARNING:
		case WARNING_CLIENT_ONLY:
			text = "\"WARNING\"";
			break;
		case ERROR:
			text = "\"ERROR\"";
			break;
		case FATAL:
			text = "\"FATAL\"";
			break;
		case PANIC:
			text = "\"PANIC\"";
			break;
		default:
			text = "\"\"";
			break;
	}

	return text;
}

/*
 * Called when a log message is not supressed by log_min_messages.
 */
static void
otel_EmitLogHook(ErrorData *edata)
{
	StringInfoData line;
	struct timeval tv;
	unsigned long long epoch_nanosec;

	gettimeofday(&tv, NULL);
	epoch_nanosec = tv.tv_sec * 1000000000 + tv.tv_usec * 1000;

	initStringInfo(&line);
	appendStringInfo(&line, "1,[%llu,%s,%d,", epoch_nanosec,
					 otel_SeverityText(edata->elevel),
					 otel_SeverityNumber(edata->elevel));

	escape_json(&line, edata->message);

	appendStringInfoString(&line, "]\n");

	otel_SendToWorker(&line);

	pfree(line.data);

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

/* Called when the module is unloaded */
void
_PG_fini(void)
{
	/* Uninstall our log processor */
	emit_log_hook = next_EmitLogHook;
}
