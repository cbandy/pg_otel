/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#ifndef PG_OTEL_CONFIG_H
#define PG_OTEL_CONFIG_H

#define PG_OTEL_CONFIG_LOGS    0x01
#define PG_OTEL_CONFIG_METRICS 0x02
#define PG_OTEL_CONFIG_TRACES  0x04

#define PG_OTEL_RESOURCE_MAX_ATTRIBUTES 128

struct otelSignalConfiguration
{
	int signals;
	char *text;
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
	char *resourceAttributes;
	char *serviceName;
};

#endif
