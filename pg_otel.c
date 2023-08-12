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
#include "pg_otel_logs.c"
#include "pg_otel_proto.c"
#include "pg_otel_worker.c"

/* Dynamically loadable module */
PG_MODULE_MAGIC;

/* Called when the module is loaded */
PGDLLEXPORT void _PG_init(void);

#if PG_VERSION_NUM < 150000
/*
 * Called when the module is unloaded, which is never.
 * - https://git.postgresql.org/gitweb/?p=postgresql.git;f=src/backend/utils/fmgr/dfmgr.c;hb=REL_11_0#l389
 */
PGDLLEXPORT void _PG_fini(void);
PGDLLEXPORT void _PG_fini(void) {}
#endif

/* BackgroundWorker entry point */
PGDLLEXPORT void otel_WorkerMain(Datum arg) pg_attribute_noreturn();

/* Hooks overridden by this module */
static emit_log_hook_type next_EmitLogHook = NULL;
#if PG_VERSION_NUM >= 150000
static shmem_request_hook_type prev_SharedMemoryRequestHook = NULL;
#endif

/* Variables set via GUC (parameters) */
static struct otelConfiguration config;

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
 * Called when a log message is not suppressed by log_min_messages.
 */
static void
otel_EmitLogHook(ErrorData *edata)
{
	/*
	 * Export log messages when configured to do so. Sending messages *from*
	 * the exporter *to* the exporter could cause a feedback loop, so don't
	 * do that. These messages still go to the next log processor which is
	 * usually PostgreSQL's built-in logging_collector or stderr.
	 */
	if (config.exports.signals & PG_OTEL_CONFIG_LOGS && MyProcPid != worker.pid)
		otel_SendLogMessage(&worker.ipc, edata);

	if (next_EmitLogHook)
		next_EmitLogHook(edata);
}

/*
 * Called after client backends and background workers have stopped, when
 * postmaster is shutting down.
 */
static void
otel_ProcExitHook(int code, Datum arg)
{
	/* Should already be the case, but check anyway */
	if (MyProcPid != PostmasterPid)
		return;

	/*
	 * Some telemetry data is emitted even after the background worker has
	 * stopped. Notice when any further data is from postmaster itself, and
	 * flush the pipe.
	 */
	worker.pid = MyProcPid;
	otel_CloseWrite(&worker.ipc);
	otel_WorkerDrain(&worker, &config);

	/*
	 * Finish with libcurl. It was initialized during [_PG_init].
	 * - https://curl.se/libcurl/c/libcurl.html#GLOBAL
	 */
	curl_global_cleanup();
}

/*
 * Since PostgreSQL 15, this hook is called after shared_preload_libraries
 * are loaded (so their GUCs exist) and before shared memory and semaphores
 * are initialized. Prior to PostgreSQL 15, modules must do this work in their
 * own [_PG_init].
 */
static void
otel_SharedMemoryRequestHook(void)
{
#if PG_VERSION_NUM >= 150000
	if (prev_SharedMemoryRequestHook)
		prev_SharedMemoryRequestHook();
#endif

	/* no-op */
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
	 * - https://curl.se/libcurl/c/libcurl.html#GLOBAL
	 */
	if (curl_global_init(CURL_GLOBAL_ALL) != CURLE_OK)
		ereport(ERROR, (errmsg("unable to initialize libcurl")));

	otel_DefineCustomVariables(&config);
	otel_ReadEnvironment();
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

	/* Request locks and other shared resources */
#if PG_VERSION_NUM >= 150000
	prev_SharedMemoryRequestHook = shmem_request_hook;
	shmem_request_hook = otel_SharedMemoryRequestHook;
#else
	otel_SharedMemoryRequestHook();
#endif

	/* Cleanup on postmaster exit */
	on_proc_exit(otel_ProcExitHook, 0);

	/* Install our log processor */
	next_EmitLogHook = emit_log_hook;
	emit_log_hook = otel_EmitLogHook;
}
