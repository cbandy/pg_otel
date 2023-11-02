/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#ifndef PG_OTEL_CONFIG_H
#define PG_OTEL_CONFIG_H

#include "postgres.h"
#include "nodes/nodes.h"

#define PG_OTEL_CONFIG_LOGS    0x01
#define PG_OTEL_CONFIG_METRICS 0x02
#define PG_OTEL_CONFIG_TRACES  0x04

#define PG_OTEL_LOG_RECORD_MAX_ATTRIBUTES 20
#define PG_OTEL_RESOURCE_MAX_ATTRIBUTES 128
#define PG_OTEL_SPAN_MAX_ATTRIBUTES 20

struct otelBaggageConfiguration
{
	char *parsed;
	char *text;
};
struct otelSignalConfiguration
{
	int signals;
	char *text;
};
struct otelTraceContextConfiguration
{
	uint8_t traceID[16];
	uint8_t parentID[8];
	uint8_t traceFlags;
	bool parsed;

	char *textTraceparent;
	char *textTracestate;
};
struct otlpConfiguration
{
	char *endpoint;
	char *protocol;
	int timeoutMS;
};
struct otelConfiguration
{
	int attributeCountLimit;
	int attributeValueLengthLimit;
	struct otelSignalConfiguration exports;
	struct otlpConfiguration otlp;
	struct otlpConfiguration otlpLogs;
	struct otlpConfiguration otlpTrace;
	struct otelBaggageConfiguration resourceAttributes;
	char *serviceName;
	struct otelTraceContextConfiguration traceContext;
};

static struct otelConfiguration config;

static bool
otel_IsSetTraceContext(Node *stmt);

#endif
