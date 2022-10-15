/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include "postgres.h"

/* Required of a background worker */
#include "miscadmin.h"
#include "postmaster/bgworker.h"
#include "postmaster/interrupt.h"
#include "storage/ipc.h"
#include "storage/latch.h"
#include "storage/lwlock.h"
#include "storage/proc.h"
#include "storage/shmem.h"

#include "pg_otel_config.c"
#include "pg_otel_proto.c"

/* Dynamically loadable module */
PG_MODULE_MAGIC;

/* Hooks called when the module is (un)loaded */
void _PG_init(void);
void _PG_fini(void);

/* Variables set via GUC (parameters) */
static struct otelConfiguration config;
static struct otelResource resource;

/* Called when the module is loaded */
void
_PG_init(void)
{
	if (!process_shared_preload_libraries_in_progress)
		return;

	otel_DefineCustomVariables(&config);
	otel_ReadEnvironment();
	otel_LoadResource(&config, &resource);
}

/*
 * Called when the module is unloaded, which is never.
 * - https://git.postgresql.org/gitweb/?p=postgresql.git;f=src/backend/utils/fmgr/dfmgr.c;hb=REL_10_0#l389
 */
void
_PG_fini(void)
{
}
