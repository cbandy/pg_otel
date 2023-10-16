/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#ifndef PG_OTEL_PROTO_H
#define PG_OTEL_PROTO_H

#include "opentelemetry/proto/collector/logs/v1/logs_service.pb-c.h"
#include "opentelemetry/proto/collector/trace/v1/trace_service.pb-c.h"
#include "opentelemetry/proto/resource/v1/resource.pb-c.h"

#include "pg_otel_config.h"

#define OTEL_FUNC_PROTO(name)    opentelemetry__proto__ ## name
#define OTEL_FUNC_COMMON(name)   OTEL_FUNC_PROTO(common__v1__ ## name)
#define OTEL_FUNC_LOGS(name)     OTEL_FUNC_PROTO(logs__v1__ ## name)
#define OTEL_FUNC_RESOURCE(name) OTEL_FUNC_PROTO(resource__v1__ ## name)
#define OTEL_FUNC_TRACE(name)    OTEL_FUNC_PROTO(trace__v1__ ## name)
#define OTEL_FUNC_EXPORT_LOGS(name) \
	OTEL_FUNC_PROTO(collector__logs__v1__export_logs_service_ ## name)

#define OTEL_TYPE_PROTO(name)    Opentelemetry__Proto__ ## name
#define OTEL_TYPE_COMMON(name)   OTEL_TYPE_PROTO(Common__V1__ ## name)
#define OTEL_TYPE_LOGS(name)     OTEL_TYPE_PROTO(Logs__V1__ ## name)
#define OTEL_TYPE_RESOURCE(name) OTEL_TYPE_PROTO(Resource__V1__ ## name)
#define OTEL_TYPE_TRACE(name)    OTEL_TYPE_PROTO(Trace__V1__ ## name)
#define OTEL_TYPE_EXPORT_LOGS(name) \
	OTEL_TYPE_PROTO(Collector__Logs__V1__ExportLogsService ## name)

#define OTEL_SEVERITY_NUMBER(name) \
	OPENTELEMETRY__PROTO__LOGS__V1__SEVERITY_NUMBER__SEVERITY_NUMBER_ ## name

#define OTEL_SPAN_KIND(name) \
	OPENTELEMETRY__PROTO__TRACE__V1__SPAN__SPAN_KIND__SPAN_KIND_ ## name

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
otel_LogAttributeInt(struct otelLogRecord *r, const char *key, int64_t value);

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

struct otelSpan
{
	OTEL_TYPE_TRACE(Span)       span;
	OTEL_TYPE_TRACE(Status)     status;
	OTEL_TYPE_COMMON(AnyValue)  attrAnyValues[PG_OTEL_SPAN_MAX_ATTRIBUTES];
	OTEL_TYPE_COMMON(KeyValue)  attrKeyValues[PG_OTEL_SPAN_MAX_ATTRIBUTES];
	OTEL_TYPE_COMMON(KeyValue) *attrList[PG_OTEL_SPAN_MAX_ATTRIBUTES];

	uint8_t id[8];
	uint8_t parent[8];
	uint8_t trace[16];
};

static void
otel_InitSpan(struct otelSpan *s)
{
	Assert(s != NULL);

	OTEL_FUNC_TRACE(span__init)(&s->span);
	OTEL_FUNC_TRACE(status__init)(&s->status);

	s->span.attributes = s->attrList;
	s->span.span_id.data = s->id;
	s->span.span_id.len = sizeof(s->id);
	s->span.trace_id.data = s->trace;
	s->span.trace_id.len = sizeof(s->trace);
}

#endif
