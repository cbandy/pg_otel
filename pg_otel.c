/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include "postgres.h"

#include "miscadmin.h"
#include "postmaster/bgworker.h"
#include "storage/ipc.h"
#include "storage/latch.h"
#include "storage/lwlock.h"
#include "utils/elog.h"

#include "curl/curl.h"

#include "pg_otel_config.c"
#include "pg_otel_proto.c"

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

/* Variables set via GUC (parameters) */
static struct otelConfiguration config;
static struct otelResource resource;

/* Called when the module is loaded */
void
_PG_init(void)
{
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
}
