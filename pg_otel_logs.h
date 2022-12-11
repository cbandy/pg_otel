/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#ifndef PG_OTEL_LOGS_H
#define PG_OTEL_LOGS_H

#include "postgres.h"
#include "lib/ilist.h"
#include "nodes/pg_list.h"
#include "utils/palloc.h"

#include "curl/curl.h"

#include "pg_otel_config.h"
#include "pg_otel_proto.h"

/*
 * otelLogsBatch is a dlist_node of one memory context containing a list of
 * ResourceLogs. That list can be sent as a single ExportLogsServiceRequest.
 */
struct otelLogsBatch
{
	dlist_node list_node;

	MemoryContext context;
	ProtobufCAllocator allocator;

	int length, capacity, dropped;
	OTEL_TYPE_LOGS(LogRecord) **records;

	List *resourceLogs; /* OTEL_TYPE_LOGS(ResourceLogs) */
};
struct otelLogsExporter
{
	dlist_head queue; /* struct otelLogsBatch */
	int batchMax, queueLength, queueMax;

	char *endpoint;
	bool  insecure;
	int   timeoutMS;
	struct otelResource resource;
};

static void
otel_InitLogsExporter(struct otelLogsExporter *exporter,
					  const struct otelConfiguration *config);

static void
otel_LoadLogsConfig(struct otelLogsExporter *exporter,
					const struct otelConfiguration *config);

static void
otel_ReceiveLogMessage(struct otelLogsExporter *exporter,
					   const uint8_t *message, size_t size);

static void
otel_SendLogsToCollector(struct otelLogsExporter *exporter, CURL *http);

#endif
