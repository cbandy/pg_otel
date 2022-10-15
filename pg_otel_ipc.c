/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include <unistd.h>

#include "postgres.h"
#include "miscadmin.h"
#include "lib/stringinfo.h"
#include "port/pg_bitutils.h"
#include "postmaster/syslogger.h"
#include "utils/elog.h"

#include "pg_otel_ipc.h"

/*
 * Write message to ipc in atomic chunks.
 */
static void
otel_SendOverIPC(struct otelIPC *ipc, bits8 signal, uint8_t *message, size_t size)
{
	PipeProtoChunk chunk;
	int rc;

	Assert(ipc != NULL);
	Assert(message != NULL);
	Assert(size > 0);

	chunk.proto.nuls[0] = chunk.proto.nuls[1] = '\0';
	chunk.proto.pid = MyProcPid;
#if PG_VERSION_NUM >= 150000
	chunk.proto.flags = signal;
#else
	chunk.proto.is_last = signal;
#endif

	/* Write all but the last chunk */
	while (size > PIPE_MAX_PAYLOAD)
	{
		chunk.proto.len = PIPE_MAX_PAYLOAD;
		memcpy(chunk.proto.data, message, PIPE_MAX_PAYLOAD);
#ifndef WIN32
		rc = write(ipc->pipe[1], &chunk, PIPE_HEADER_SIZE + PIPE_MAX_PAYLOAD);
		(void) rc;
#endif
		message += PIPE_MAX_PAYLOAD;
		size -= PIPE_MAX_PAYLOAD;
	}

	/* Write the last chunk */
#if PG_VERSION_NUM >= 150000
	chunk.proto.flags |= PG_OTEL_IPC_FINISHED;
#else
	chunk.proto.is_last |= PG_OTEL_IPC_FINISHED;
#endif
	chunk.proto.len = size;
	memcpy(chunk.proto.data, message, size);
	rc = write(ipc->pipe[1], &chunk, PIPE_HEADER_SIZE + size);
	(void) rc;
}

static void
otel_ProcessInput(struct otelIPC *ipc, void *opaque,
				  void (*dispatch)(void *opaque, bits8 signal,
								   const char *message, size_t size))
{
	struct Portion
	{
		int32 pid;
		StringInfoData data;
	};

	char *cursor = ipc->buffer;
	int remaining = ipc->offset;

	while (remaining >= (int) (PIPE_HEADER_SIZE + 1))
	{
		int   length;
		bits8 signal;
		PipeProtoHeader header;
		List     *portionBucket;
		List    **portionBuckets;
		ListCell *cell;
		struct Portion *empty = NULL;
		struct Portion *message = NULL;

		/* Verify the cursor points to a protocol header */
		memcpy(&header, cursor, PIPE_HEADER_SIZE);
#if PG_VERSION_NUM >= 150000
		signal = header.flags & PG_OTEL_IPC_SIGNALS;
#else
		signal = header.is_last & PG_OTEL_IPC_SIGNALS;
#endif
		if (!(header.nuls[0] == '\0' && header.nuls[1] == '\0' &&
			  header.len > 0 && header.len <= PIPE_MAX_PAYLOAD &&
			  header.pid != 0 && pg_popcount((char *) &signal, 1) == 1))
		{
			ereport(WARNING, (errmsg("unexpected otel message header")));

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

		else if (signal == PG_OTEL_IPC_LOGS)    portionBuckets = ipc->buckets[0];
		else if (signal == PG_OTEL_IPC_METRICS) portionBuckets = ipc->buckets[1];
		else if (signal == PG_OTEL_IPC_TRACES)  portionBuckets = ipc->buckets[2];
		else
			pg_unreachable();

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
		if (message == NULL && (header.flags & PG_OTEL_IPC_FINISHED))
#else
		if (message == NULL && (header.is_last & PG_OTEL_IPC_FINISHED))
#endif
		{
			/* This chunk is a complete message; return it */
			dispatch(opaque, signal, cursor + PIPE_HEADER_SIZE, header.len);

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
		if (header.flags & PG_OTEL_IPC_FINISHED)
#else
		if (header.is_last & PG_OTEL_IPC_FINISHED)
#endif
		{
			/* The message is now complete; return it */
			dispatch(opaque, signal, message->data.data, message->data.len);

			/* Mark the slot unused and reclaim storage */
			message->pid = 0;
			pfree(message->data.data);
		}

		/* On to the next chunk */
		remaining -= length;
		cursor += length;
	}

	/* We don't have a full chunk, so left-align what remains in the buffer */
	if (remaining > 0 && cursor != ipc->buffer)
		memmove(ipc->buffer, cursor, remaining);
	ipc->offset = remaining;
}

/*
 * Read zero or more messages from ipc. Each message is passed to dispatch.
 */
static void
otel_ReceiveOverIPC(struct otelIPC *ipc, void *opaque,
					void (*dispatch)(void *opaque, bits8 signal,
									 const char *message, size_t size))
{
	int n = 0;

	Assert(ipc != NULL);
	Assert(dispatch != NULL);

#ifndef WIN32
	n = read(ipc->pipe[0],
			 ipc->buffer + ipc->offset,
			 sizeof(ipc->buffer) - ipc->offset);
#endif

	if (n < 0)
	{
		if (errno != EINTR)
			ereport(LOG,
					(errcode_for_socket_access(),
					 errmsg("could not read from otel pipe: %m")));
	}
	else if (n > 0)
	{
		ipc->offset += n;
		otel_ProcessInput(ipc, opaque, dispatch);
	}
	else
	{
		/* FIXME */
		ereport(LOG, (errmsg("otel pipe EOF")));
	}
}
