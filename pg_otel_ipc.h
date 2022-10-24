/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#ifndef PG_OTEL_IPC_H
#define PG_OTEL_IPC_H

#include "postgres.h"
#include "postmaster/syslogger.h"
#include "storage/latch.h"

#define PG_OTEL_IPC_FINISHED 0x01
#define PG_OTEL_IPC_LOGS     0x10
#define PG_OTEL_IPC_METRICS  0x20
#define PG_OTEL_IPC_TRACES   0x40
#define PG_OTEL_IPC_SIGNALS (PG_OTEL_IPC_LOGS | PG_OTEL_IPC_METRICS | PG_OTEL_IPC_TRACES)

struct otelIPC
{
	List *buckets[3][256];
	char  buffer[2 * PIPE_CHUNK_SIZE];
	int   offset;

#ifndef WIN32
	int pipe[2];
#endif
};

static uint32 otel_AddReadEventToSet(struct otelIPC *ipc, WaitEventSet *set);
static void otel_CloseWrite(struct otelIPC *ipc);
static void otel_OpenIPC(struct otelIPC *ipc);

static void
otel_ReceiveOverIPC(struct otelIPC *ipc,
					void *opaque,
					void (*dispatch)(void *opaque, bits8 signal,
									 const char *message, size_t size));

static void
otel_SendOverIPC(struct otelIPC *ipc,
				 bits8 signal, uint8_t *message, size_t size);


#endif
