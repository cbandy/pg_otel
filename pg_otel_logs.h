/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#ifndef PG_OTEL_LOGS_H
#define PG_OTEL_LOGS_H

#include "postgres.h"
#include "storage/latch.h"
#include "storage/lwlock.h"
#include "utils/palloc.h"

#ifdef WIN32
#include "pthread-win32.h"
#else
#include <pthread.h>
#endif

#include "curl/curl.h"

#include "pg_otel_config.h"
#include "pg_otel_proto.h"

struct otelLogsBatch
{
	MemoryContext context;
	ProtobufCAllocator allocator;

	int length, capacity, dropped;
	OTEL_TYPE_LOGS(LogRecord) **records;
};

struct otelLogsThread
{
	sig_atomic_t volatile quit;
	pthread_t thread;

	CURL *http;
	LWLock *lock;

	struct Latch         batchLatch;
	struct otelLogsBatch batchHold, batchSend;
	struct otelResource  resource;

	char *endpoint;
	bool  insecure;
	int   timeoutMS;
};

static void
otel_InitLogsThread(struct otelLogsThread *t, LWLock *lock, int batchCapacity);

static void
otel_LoadLogsConfig(struct otelLogsThread *t, struct otelConfiguration *config);

static void
otel_ReceiveLogMessage(struct otelLogsThread *t, const char *msg, size_t size);

static void
otel_StartLogsThread(struct otelLogsThread *t);

static void
otel_WakeLogsThread(struct otelLogsThread *t);

static void
otel_WaitLogsThread(struct otelLogsThread *t);

#endif
