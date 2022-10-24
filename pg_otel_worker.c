/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include "postgres.h"
#include "miscadmin.h"

#if PG_VERSION_NUM >= 140000
#include "utils/wait_event.h"
#else
#include "pgstat.h"
#endif

#include "pg_otel_config.h"
#include "pg_otel_proto.h"
#include "pg_otel_ipc.c"

#define PG_OTEL_LWLOCKS 1

struct otelWorker
{
	sig_atomic_t volatile gotSIGHUP;
	sig_atomic_t volatile gotSIGTERM;

	struct otelIPC ipc;
	int pid;
};

struct otelWorkerThreads
{
	bits8 received;
};

static void
otel_WorkerReceive(void *ptr, bits8 signal, const char *message, size_t size)
{
	struct otelWorkerThreads *threads = ptr;

	Assert(threads != NULL);

	threads->received |= signal;
}

static void
otel_WorkerRun(struct otelWorker *worker,
			   struct otelConfiguration *config)
{
	struct otelWorkerThreads threads = {};
	uint32 readEvent = 0;
	WaitEventSet *wes;

	/* Set up a WaitEventSet for our process latch and IPC */
	wes = CreateWaitEventSet(CurrentMemoryContext, 3);
	AddWaitEventToSet(wes, WL_LATCH_SET, PGINVALID_SOCKET, MyLatch, NULL);
	AddWaitEventToSet(wes, WL_POSTMASTER_DEATH, PGINVALID_SOCKET, NULL, NULL);
	readEvent = otel_AddReadEventToSet(&worker->ipc, wes);

	for (;;)
	{
		WaitEvent event;

		/* Wait forever for some work */
		WaitEventSetWait(wes, -1, &event, 1, PG_WAIT_EXTENSION);
		ResetLatch(MyLatch);

		if (worker->gotSIGTERM)
			break;

		if (worker->gotSIGHUP)
		{
			worker->gotSIGHUP = false;
			ProcessConfigFile(PGC_SIGHUP);
		}

		if (event.events == readEvent)
		{
			threads.received = 0;
			otel_ReceiveOverIPC(&worker->ipc, &threads, otel_WorkerReceive);
		}
	}

	/* TODO: flush; look for techniques around ShutdownXLOG */
}
