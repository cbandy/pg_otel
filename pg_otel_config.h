/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#ifndef PG_OTEL_CONFIG_H
#define PG_OTEL_CONFIG_H

#define PG_OTEL_RESOURCE_MAX_ATTRIBUTES 128

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
	struct otlpConfiguration otlp;
	struct otlpConfiguration otlpLogs;
	char *resourceAttributes;
	char *serviceName;
};

#endif
