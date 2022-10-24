/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include "postgres.h"

#include "miscadmin.h"
#include "postmaster/bgworker.h"
#include "storage/ipc.h"
#include "storage/latch.h"
#include "storage/lwlock.h"
#include "utils/elog.h"

#include "curl/curl.h"

#include "pg_otel.h"
#include "pg_otel_config.c"
#include "pg_otel_proto.c"
#include "pg_otel_worker.c"

/* Dynamically loadable module */
PG_MODULE_MAGIC;

/* Called when the module is loaded */
void _PG_init(void);

#if PG_VERSION_NUM < 150000
/*
 * Called when the module is unloaded, which is never.
 * - https://git.postgresql.org/gitweb/?p=postgresql.git;f=src/backend/utils/fmgr/dfmgr.c;hb=REL_11_0#l389
 */
void _PG_fini(void);
void _PG_fini(void) {}
#endif

/* BackgroundWorker entry point */
void otel_WorkerMain(Datum arg) pg_attribute_noreturn();

/* Variables set via GUC (parameters) */
static struct otelConfiguration config;
static struct otelResource resource;

/* Values shared between backends and the background worker */
static struct otelWorker worker;

/* Signal handler; see [otel_WorkerMain] */
static void
otel_WorkerHandleSIGHUP(SIGNAL_ARGS)
{
	int save_errno = errno;
	worker.gotSIGHUP = true;
	SetLatch(MyLatch);
	errno = save_errno;
}

/* Signal handler; see [otel_WorkerMain] */
static void
otel_WorkerHandleSIGTERM(SIGNAL_ARGS)
{
	int save_errno = errno;
	worker.gotSIGTERM = true;
	SetLatch(MyLatch);
	errno = save_errno;
}

/*
 * Called in the background worker after being forked.
 */
void
otel_WorkerMain(Datum arg)
{
	/* Register our signal handlers */
	pqsignal(SIGHUP, otel_WorkerHandleSIGHUP);
	pqsignal(SIGTERM, otel_WorkerHandleSIGTERM);

	/* Ready to receive signals */
	BackgroundWorkerUnblockSignals();

	/* Notice when telemetry data is from the worker itself */
	worker.pid = MyProcPid;

	otel_CloseWrite(&worker.ipc);
	otel_WorkerRun(&worker, &config);

	/* Exit zero so we aren't restarted */
	proc_exit(0);
}

/* Called when the module is loaded */
void
_PG_init(void)
{
	BackgroundWorker exporter;

	if (!process_shared_preload_libraries_in_progress)
		return;

	/*
	 * Initialize libcurl as soon as possible; not all versions are thread-safe.
	 * TODO: Call curl_global_cleanup before postgres terminates?
	 * - https://curl.se/libcurl/c/libcurl.html#GLOBAL
	 */
	if (curl_global_init(CURL_GLOBAL_ALL) != CURLE_OK)
		ereport(ERROR, (errmsg("unable to initialize libcurl")));

	otel_DefineCustomVariables(&config);
	otel_ReadEnvironment();
	otel_LoadResource(&config, &resource);
	otel_OpenIPC(&worker.ipc);

	/*
	 * Register our background worker to start immediately. Restart it without
	 * delay if it crashes.
	 */
	MemSet(&exporter, 0, sizeof(BackgroundWorker));
	exporter.bgw_flags = BGWORKER_SHMEM_ACCESS;
	exporter.bgw_start_time = BgWorkerStart_PostmasterStart;
	snprintf(exporter.bgw_name, BGW_MAXLEN, "OpenTelemetry exporter");
	snprintf(exporter.bgw_library_name, BGW_MAXLEN, PG_OTEL_LIBRARY);
	snprintf(exporter.bgw_function_name, BGW_MAXLEN, "otel_WorkerMain");
	RegisterBackgroundWorker(&exporter);
}
