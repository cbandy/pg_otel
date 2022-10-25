/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#ifndef PG_OTEL_PROTO_H
#define PG_OTEL_PROTO_H

#include "opentelemetry/proto/collector/logs/v1/logs_service.pb-c.h"
#include "opentelemetry/proto/resource/v1/resource.pb-c.h"

#include "pg_otel_config.h"

#define OTEL_FUNC_PROTO(name)    opentelemetry__proto__ ## name
#define OTEL_FUNC_COMMON(name)   OTEL_FUNC_PROTO(common__v1__ ## name)
#define OTEL_FUNC_LOGS(name)     OTEL_FUNC_PROTO(logs__v1__ ## name)
#define OTEL_FUNC_RESOURCE(name) OTEL_FUNC_PROTO(resource__v1__ ## name)
#define OTEL_FUNC_EXPORT_LOGS(name) \
	OTEL_FUNC_PROTO(collector__logs__v1__export_logs_service_ ## name)

#define OTEL_TYPE_PROTO(name)    Opentelemetry__Proto__ ## name
#define OTEL_TYPE_COMMON(name)   OTEL_TYPE_PROTO(Common__V1__ ## name)
#define OTEL_TYPE_LOGS(name)     OTEL_TYPE_PROTO(Logs__V1__ ## name)
#define OTEL_TYPE_RESOURCE(name) OTEL_TYPE_PROTO(Resource__V1__ ## name)
#define OTEL_TYPE_EXPORT_LOGS(name) \
	OTEL_TYPE_PROTO(Collector__Logs__V1__ExportLogsService ## name)

#define OTEL_SEVERITY_NUMBER(name) \
	OPENTELEMETRY__PROTO__LOGS__V1__SEVERITY_NUMBER__SEVERITY_NUMBER_ ## name

#define OTEL_VALUE_CASE(name) \
	OPENTELEMETRY__PROTO__COMMON__V1__ANY_VALUE__VALUE_ ## name ## _VALUE

static void otel_InitProtobufCAllocator(ProtobufCAllocator *, MemoryContext);

struct otelLogRecord
{
	OTEL_TYPE_LOGS(LogRecord)   record;
	OTEL_TYPE_COMMON(AnyValue)  attrAnyValues[PG_OTEL_LOG_RECORD_MAX_ATTRIBUTES];
	OTEL_TYPE_COMMON(KeyValue)  attrKeyValues[PG_OTEL_LOG_RECORD_MAX_ATTRIBUTES];
	OTEL_TYPE_COMMON(KeyValue) *attrList[PG_OTEL_LOG_RECORD_MAX_ATTRIBUTES];
	OTEL_TYPE_COMMON(AnyValue)  body;
};

static void
otel_InitLogRecord(struct otelLogRecord *r)
{
	Assert(r != NULL);

	OTEL_FUNC_LOGS(log_record__init)(&r->record);
	OTEL_FUNC_COMMON(any_value__init)(&r->body);

	r->record.attributes = r->attrList;
	r->record.body = &r->body;
}

static void
otel_LogAttributeInt(struct otelLogRecord *r, const char *key, int value);

static void
otel_LogAttributeStr(struct otelLogRecord *r, const char *key, const char *value);

struct otelResource
{
	OTEL_TYPE_RESOURCE(Resource) resource;
	OTEL_TYPE_COMMON(AnyValue)   attrAnyValues[PG_OTEL_RESOURCE_MAX_ATTRIBUTES];
	OTEL_TYPE_COMMON(KeyValue)   attrKeyValues[PG_OTEL_RESOURCE_MAX_ATTRIBUTES];
	OTEL_TYPE_COMMON(KeyValue)  *attrList[PG_OTEL_RESOURCE_MAX_ATTRIBUTES];
};

static void
otel_InitResource(struct otelResource *r)
{
	Assert(r != NULL);

	OTEL_FUNC_RESOURCE(resource__init)(&r->resource);

	r->resource.attributes = r->attrList;
}

static void
otel_LoadResource(const struct otelConfiguration *src, struct otelResource *dst);

#endif
