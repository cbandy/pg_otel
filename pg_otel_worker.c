/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include "postgres.h"
#include "miscadmin.h"

#if PG_VERSION_NUM >= 140000
#include "utils/wait_event.h"
#else
#include "pgstat.h"
#endif

#include "pg_otel_config.h"
#include "pg_otel_ipc.c"
#include "pg_otel_logs.h"
#include "pg_otel_traces.h"

struct otelWorker
{
	sig_atomic_t volatile gotSIGHUP;
	sig_atomic_t volatile gotSIGTERM;

	struct otelIPC ipc;
	int pid;
};

struct otelWorkerExporter
{
	struct otelLogsExporter logs;
	struct otelTraceExporter traces;
};

static void
otel_WorkerReceive(void *ptr, bits8 signal, const uint8_t *message, size_t size)
{
	struct otelWorkerExporter *exporter = ptr;

	Assert(exporter != NULL);

	if (signal & PG_OTEL_IPC_LOGS)
		otel_ReceiveLogMessage(&exporter->logs, message, size);

	if (signal & PG_OTEL_IPC_TRACES)
		otel_ReceiveSpan(&exporter->traces, message, size);
}

static bool
otel_WorkerReadIPC(struct otelIPC *ipc, struct otelWorkerExporter *exporter, CURL *http)
{
	Assert(exporter != NULL);
	Assert(http != NULL);
	Assert(ipc != NULL);

	otel_ReceiveOverIPC(ipc, exporter, otel_WorkerReceive);

	if (exporter->logs.queueLength > 0)
		otel_SendLogsToCollector(&exporter->logs, http);

	if (exporter->traces.queueLength > 0)
		otel_SendSpansToCollector(&exporter->traces, http);

	return
		exporter->logs.queueLength == 0 &&
		exporter->traces.queueLength == 0 &&
		otel_IPCIsIdle(ipc);
}

static void
otel_WorkerDrain(struct otelWorker *worker, struct otelConfiguration *config)
{
	struct otelWorkerExporter exporter = {};
	CURL *http = curl_easy_init();

	if (http == NULL)
		ereport(FATAL, (errmsg("could not initialize curl for otel exporter")));

	otel_InitLogsExporter(&exporter.logs, config);
	otel_InitTraceExporter(&exporter.traces, config);

	for (;;)
	{
		if (otel_WorkerReadIPC(&worker->ipc, &exporter, http))
			break;
	}

	curl_easy_cleanup(http);
}

static void
otel_WorkerRun(struct otelWorker *worker, struct otelConfiguration *config)
{
	struct otelWorkerExporter exporter = {};
	uint32 readEvent = 0;
	WaitEventSet *wes;
	CURL *http = curl_easy_init();

	if (http == NULL)
		ereport(FATAL, (errmsg("could not initialize curl for otel exporter")));

	otel_InitLogsExporter(&exporter.logs, config);
	otel_InitTraceExporter(&exporter.traces, config);

	/* Set up a WaitEventSet for our process latch and IPC */
	wes = CreateWaitEventSet(CurrentMemoryContext, 3);
	AddWaitEventToSet(wes, WL_LATCH_SET, PGINVALID_SOCKET, MyLatch, NULL);
	AddWaitEventToSet(wes, WL_POSTMASTER_DEATH, PGINVALID_SOCKET, NULL, NULL);
	readEvent = otel_AddReadEventToSet(&worker->ipc, wes);

	for (;;)
	{
		WaitEvent event = {};
		bool idle = true;

		/* Wait one second for some work */
		WaitEventSetWait(wes, 1000, &event, 1, PG_WAIT_EXTENSION);
		ResetLatch(MyLatch);

		if (worker->gotSIGHUP)
		{
			worker->gotSIGHUP = false;
			ProcessConfigFile(PGC_SIGHUP);

			otel_LoadLogsConfig(&exporter.logs, config);
			otel_LoadTraceConfig(&exporter.traces, config);
		}

		if (event.events == readEvent)
			idle = otel_WorkerReadIPC(&worker->ipc, &exporter, http);

		/*
		 * Stop when the queues are empty and the IPC channel can be handed off
		 * to postmaster.
		 */
		if (worker->gotSIGTERM && idle)
			break;
	}

	curl_easy_cleanup(http);
}
