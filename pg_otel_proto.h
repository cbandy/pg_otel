/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#ifndef PG_OTEL_PROTO_H
#define PG_OTEL_PROTO_H

#include "opentelemetry/proto/resource/v1/resource.pb-c.h"
#include "pg_otel_config.h"

#define OTEL_FUNC_PROTO(name)    opentelemetry__proto__ ## name
#define OTEL_FUNC_COMMON(name)   OTEL_FUNC_PROTO(common__v1__ ## name)
#define OTEL_FUNC_RESOURCE(name) OTEL_FUNC_PROTO(resource__v1__ ## name)

#define OTEL_TYPE_PROTO(name)    Opentelemetry__Proto__ ## name
#define OTEL_TYPE_COMMON(name)   OTEL_TYPE_PROTO(Common__V1__ ## name)
#define OTEL_TYPE_RESOURCE(name) OTEL_TYPE_PROTO(Resource__V1__ ## name)

#define OTEL_VALUE_CASE(name) \
	OPENTELEMETRY__PROTO__COMMON__V1__ANY_VALUE__VALUE_ ## name ## _VALUE

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
