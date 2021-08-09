/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include <stdio.h>

#include "postgres.h"
#include "fmgr.h"

/* Dynamically loadable module */
PG_MODULE_MAGIC;

/* Hooks called when the module is (un)loaded */
void _PG_init(void);
void _PG_fini(void);


void
_PG_init(void)
{
}

void
_PG_fini(void)
{
}
