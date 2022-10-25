/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include "postgres.h"
#include "utils/guc.h"
#include "utils/palloc.h"

#include "pg_otel.h"
#include "pg_otel_proto.h"

/* Allocate size memory, as needed by [ProtobufCAllocator.alloc] */
static void *
otel_ProtobufAllocatorAlloc(void *context, size_t size) {
	return MemoryContextAlloc(context, size);
}

/* Free memory at pointer, as needed by [ProtobufCAllocator.free] */
static void
otel_ProtobufAllocatorFree(void *context, void *pointer) {
	pfree(pointer);
}

/* Initialize allocator to allocate within ctx */
static void
otel_InitProtobufCAllocator(ProtobufCAllocator *allocator, MemoryContext ctx)
{
	Assert(allocator != NULL);

	allocator->allocator_data = ctx;
	allocator->alloc = otel_ProtobufAllocatorAlloc;
	allocator->free = otel_ProtobufAllocatorFree;
}

static void
otel_AttributeInt(OTEL_TYPE_COMMON(AnyValue) *anyValues,
				  OTEL_TYPE_COMMON(KeyValue) *keyValues,
				  OTEL_TYPE_COMMON(KeyValue) **attributes,
				  size_t *n_attributes, const char *key, int value)
{
	size_t i = *n_attributes;

	OTEL_FUNC_COMMON(any_value__init)(&anyValues[i]);
	anyValues[i].int_value = value;
	anyValues[i].value_case = OTEL_VALUE_CASE(INT);

	OTEL_FUNC_COMMON(key_value__init)(&keyValues[i]);
	keyValues[i].key = (char *)key;
	keyValues[i].value = &anyValues[i];

	attributes[i] = &keyValues[i];
	*n_attributes += 1;
}

static void
otel_AttributeStr(OTEL_TYPE_COMMON(AnyValue) *anyValues,
				  OTEL_TYPE_COMMON(KeyValue) *keyValues,
				  OTEL_TYPE_COMMON(KeyValue) **attributes,
				  size_t *n_attributes, const char *key, const char *value)
{
	size_t i = *n_attributes;

	OTEL_FUNC_COMMON(any_value__init)(&anyValues[i]);
	anyValues[i].string_value = (char *)value;
	anyValues[i].value_case = OTEL_VALUE_CASE(STRING);

	OTEL_FUNC_COMMON(key_value__init)(&keyValues[i]);
	keyValues[i].key = (char *)key;
	keyValues[i].value = &anyValues[i];

	attributes[i] = &keyValues[i];
	*n_attributes += 1;
}

static int
otel_CompareKeyValueKeys(const void *a, const void *b)
{
	const OTEL_TYPE_COMMON(KeyValue) *kva = a;
	const OTEL_TYPE_COMMON(KeyValue) *kvb = b;

	return strcmp(kva->key, kvb->key);
}

static void
otel_LogAttributeInt(struct otelLogRecord *r, const char *key, int value)
{
	Assert(r != NULL);
	Assert(r->record.n_attributes < PG_OTEL_LOG_RECORD_MAX_ATTRIBUTES);

	otel_AttributeInt(r->attrAnyValues,
					  r->attrKeyValues,
					  r->record.attributes,
					  &r->record.n_attributes,
					  key, value);
}

static void
otel_LogAttributeStr(struct otelLogRecord *r, const char *key, const char *value)
{
	Assert(r != NULL);
	Assert(r->record.n_attributes < PG_OTEL_LOG_RECORD_MAX_ATTRIBUTES);

	otel_AttributeStr(r->attrAnyValues,
					  r->attrKeyValues,
					  r->record.attributes,
					  &r->record.n_attributes,
					  key, value);
}

static void
otel_ResourceAttributeStr(struct otelResource *r,
						  const char *key, const char *value)
{
	OTEL_TYPE_COMMON(KeyValue) search = { .key = (char *)key };
	OTEL_TYPE_COMMON(KeyValue) *found = NULL;

	Assert(r != NULL);

	found = (OTEL_TYPE_COMMON(KeyValue) *)
		bsearch((void *)&search,
				(void *)r->attrKeyValues, r->resource.n_attributes,
				sizeof(OTEL_TYPE_COMMON(KeyValue)), otel_CompareKeyValueKeys);

	if (found != NULL)
	{
		free(found->value->string_value);
		found->value->string_value = strdup(value);
	}
	else if (r->resource.n_attributes < PG_OTEL_RESOURCE_MAX_ATTRIBUTES)
	{
		otel_AttributeStr(r->attrAnyValues,
						  r->attrKeyValues,
						  r->resource.attributes,
						  &r->resource.n_attributes,
						  key, strdup(value));

		qsort((void *)r->attrKeyValues, r->resource.n_attributes,
			  sizeof(OTEL_TYPE_COMMON(KeyValue)), otel_CompareKeyValueKeys);
	}
	else
		r->resource.dropped_attributes_count++;
}

static void
otel_LoadResource(const struct otelConfiguration *src, struct otelResource *dst)
{
	size_t i;
	const char *value;

	Assert(dst != NULL);
	Assert(dst->resource.n_attributes < PG_OTEL_RESOURCE_MAX_ATTRIBUTES);

	/* Free all existing string values */
	for (i = 0; i < dst->resource.n_attributes; i++)
	{
		if (dst->attrAnyValues[i].value_case == OTEL_VALUE_CASE(STRING))
			free(dst->attrAnyValues[i].string_value);
	}

	otel_InitResource(dst);

	/*
	 * Populate dst with attributes according to OpenTelemetry Semantic Conventions.
	 * - https://docs.opentelemetry.io/reference/specification/resource/sdk/
	 * - https://docs.opentelemetry.io/reference/specification/resource/semantic_conventions/
	 *
	 * First, add attributes that MUST be provided by the SDK so they aren't
	 * dropped due to attribute limits.
	 */
	otel_ResourceAttributeStr(dst, "service.name", src->serviceName);

	/*
	 * Add attributes that can be overridden by src.
	 */
	value = GetConfigOption("server_version", false, false);
	if (value != NULL)
		otel_ResourceAttributeStr(dst, "service.version", value);

	/*
	 * Add attributes from src->resourceAttributes, if any.
	 */
	// TODO: Baggage!

	/*
	 * Finally, add attributes that cannot be overridden by src.
	 */
	otel_ResourceAttributeStr(dst, "service.name", src->serviceName);
	otel_ResourceAttributeStr(dst, "telemetry.sdk.name", PG_OTEL_LIBRARY);
	otel_ResourceAttributeStr(dst, "telemetry.sdk.version", PG_OTEL_VERSION);
}
