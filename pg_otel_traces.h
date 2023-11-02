/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#ifndef PG_OTEL_TRACES_H
#define PG_OTEL_TRACES_H

#include "pg_otel_proto.h"

/*
 * otelTraceBatch is a dlist_node of one memory context containing a list of
 * ResourceSpans. That list can be sent as a single ExportTraceServiceRequest.
 */
struct otelTraceBatch
{
	dlist_node list_node;

	MemoryContext context;
	ProtobufCAllocator allocator;

	int length, capacity, dropped;
	OTEL_TYPE_TRACE(Span) **spans;

	List *resourceSpans; /* OTEL_TYPE_TRACE(ResourceSpans) */
};
struct otelTraceExporter
{
	dlist_head queue; /* struct otelTraceBatch */
	int batchMax, queueLength, queueMax;

	char *endpoint;
	bool  insecure;
	int   timeoutMS;
	struct otelResource resource;
};


static void
otel_EndQuerySpan(struct otelSpan *span, const QueryDesc *query);

static void
otel_EndUtilitySpan(struct otelSpan *span, ProcessUtilityContext context,
					const PlannedStmt *planned);

static void
otel_SendSpan(struct otelIPC *ipc, const struct otelSpan *span);

static struct otelSpan *
otel_StartSpan(MemoryContext ctx, const struct otelSpan *parent,
			   const PlannedStmt *planned, const char *statement);

static void
otel_InitTraceExporter(struct otelTraceExporter *exporter,
					   const struct otelConfiguration *config);

static void
otel_LoadTraceConfig(struct otelTraceExporter *exporter,
					 const struct otelConfiguration *config);

static void
otel_ReceiveSpan(struct otelTraceExporter *exporter,
				 const uint8_t *message, size_t size);

static void
otel_SendSpansToCollector(struct otelTraceExporter *exporter, CURL *http);

static struct otelSpan *currentSpan;

#endif
