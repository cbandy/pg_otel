/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include "postgres.h"
#include "utils/guc.h"
#include "utils/varlena.h"

#include "pg_otel_config.h"

#if PG_VERSION_NUM < 160000
/* https://git.postgresql.org/gitweb/?p=postgresql.git;h=0a20ff54f5e661589 */
#include "utils/elog.h"
static void *guc_malloc(int elevel, size_t size)
{
	void *data = malloc(size);
	if (data == NULL)
		ereport(elevel,
				(errcode(ERRCODE_OUT_OF_MEMORY),
				 errmsg("out of memory")));
	return data;
}
#endif

/*
 * The GUC system expects variables to be assigned statically, so hooks are
 * called without any destination pointers. We pretty much do the same.
 * Pass some pointers from [otel_DefineCustomVariables] to hooks through here.
 */
static struct otelSignalConfiguration *gucExportsHook = NULL;

static bool
otel_CheckBaggage(char **next, void **extra, GucSource source)
{
	/* TODO: https://www.w3.org/TR/baggage/ */
	return true;
}

static bool
otel_CheckEndpoint(char **next, void **extra, GucSource source)
{
	/* TODO: https://curl.se/libcurl/c/curl_url.html */

	if (strncmp(*next, "http://", 7) != 0 &&
		strncmp(*next, "https://", 8) != 0)
	{
		GUC_check_errdetail("URL must begin with http or https.");
		return false;
	}

	return true;
}

static bool
otel_CheckExports(char **next, void **extra, GucSource source)
{
	struct otelSignalConfiguration parsed = {};

	ListCell *cell = NULL;
	List     *list = NULL;

	parsed.text = pstrdup(*next);
	if (!SplitIdentifierString(parsed.text, ',', &list))
	{
		GUC_check_errdetail("List syntax is invalid.");
		pfree(parsed.text);
		list_free(list);
		return false;
	}

	foreach(cell, list)
	{
		char *item = (char *) lfirst(cell);

		if (pg_strcasecmp(item, "logs") == 0 ||
			pg_strcasecmp(item, "log") == 0)
			parsed.signals |= PG_OTEL_CONFIG_LOGS;
		else
		{
			GUC_check_errdetail("Unrecognized signal: \"%s\".", item);
			pfree(parsed.text);
			list_free(list);
			return false;
		}
	}

	pfree(parsed.text);
	parsed.text = NULL;
	list_free(list);

	/* This will be freed by PostgreSQL GUC */
	*extra = guc_malloc(ERROR, sizeof(struct otelSignalConfiguration));
	memcpy(*extra, &parsed, sizeof(struct otelSignalConfiguration));
	return true;
}

static void
otel_AssignExports(const char *next, void *extra)
{
	struct otelSignalConfiguration *parsed =
		(struct otelSignalConfiguration *) extra;

	gucExportsHook->signals = parsed->signals;
}

static bool
otel_CheckServiceName(char **next, void **extra, GucSource source)
{
	if (*next == NULL || *next[0] == '\0')
	{
		GUC_check_errdetail("resource attribute \"service.name\" cannot be blank.");
		return false;
	}

	return true;
}

static void
otel_CustomVariableEnv(const char *opt, const char *env)
{
	char *value = getenv(env);

	/*
	 * https://docs.opentelemetry.io/reference/specification/sdk-environment-variables/#parsing-empty-value
	 *
	 * > The SDK MUST interpret an empty value of an environment variable
	 * > the same way as when the variable is unset.
	 */
	if (value != NULL && value[0] != '\0')
		SetConfigOption(opt, value, PGC_POSTMASTER, PGC_S_ENV_VAR);
}

static void
otel_DefineCustomVariables(struct otelConfiguration *dst)
{
	DefineCustomIntVariable
		("otel.attribute_count_limit",
		 "Maximum attributes allowed on each signal",
		 NULL,

		 &dst->attributeCountLimit,
		 PG_OTEL_RESOURCE_MAX_ATTRIBUTES,
		 PG_OTEL_RESOURCE_MAX_ATTRIBUTES,
		 PG_OTEL_RESOURCE_MAX_ATTRIBUTES,

		 PGC_INTERNAL, 0, NULL, NULL, NULL);

	gucExportsHook = &dst->exports;
	DefineCustomStringVariable
		("otel.export",
		 "Signals to export over OTLP",
		 "May be empty or \"logs\".",

		 &dst->exports.text,
		 "",

		 PGC_SIGHUP, GUC_LIST_INPUT,
		 otel_CheckExports, otel_AssignExports, NULL);

	DefineCustomStringVariable
		("otel.otlp_endpoint",
		 "Target URL to which the exporter sends signals",

		 "A scheme of https indicates a secure connection."
		 " The per-signal endpoint configuration options take precedence.",

		 &dst->otlp.endpoint,
		 "http://localhost:4318",

		 PGC_SIGHUP, 0, otel_CheckEndpoint, NULL, NULL);

	DefineCustomStringVariable
		("otel.otlp_protocol",
		 "The exporter transport protocol",
		 NULL,

		 &dst->otlp.protocol,
		 "http/protobuf",

		 PGC_INTERNAL, 0, NULL, NULL, NULL);

	DefineCustomIntVariable
		("otel.otlp_timeout",
		 "Maximum time the exporter will wait for each batch export",
		 NULL,

		 &dst->otlp.timeoutMS,
		 10 * 1000L, 1, 60 * 60 * 1000L, /* 10sec; between 1ms and 60min */

		 PGC_SIGHUP, GUC_UNIT_MS, NULL, NULL, NULL);

	DefineCustomStringVariable
		("otel.resource_attributes",
		 "Key-value pairs to be used as resource attributes",

		 "Formatted as W3C Baggage.",

		 &dst->resourceAttributes,
		 NULL,

		 PGC_SIGHUP, 0, otel_CheckBaggage, NULL, NULL);

	DefineCustomStringVariable
		("otel.service_name",
		 "Logical name of this service",
		 NULL,

		 &dst->serviceName,
		 "postgresql",

		 PGC_SIGHUP, 0, otel_CheckServiceName, NULL, NULL);

#if PG_VERSION_NUM >= 150000
	MarkGUCPrefixReserved("otel");
#else
	EmitWarningsOnPlaceholders("otel");
#endif
}

static void
otel_ReadEnvironment(void)
{
	/*
	 * https://docs.opentelemetry.io/reference/specification/sdk-environment-variables/#attribute-limits
	 */
	otel_CustomVariableEnv("otel.attribute_count_limit", "OTEL_ATTRIBUTE_COUNT_LIMIT");

	/*
	 * https://docs.opentelemetry.io/reference/specification/protocol/exporter/
	 */
	otel_CustomVariableEnv("otel.otlp_endpoint", "OTEL_EXPORTER_OTLP_ENDPOINT");
	otel_CustomVariableEnv("otel.otlp_protocol", "OTEL_EXPORTER_OTLP_PROTOCOL");
	otel_CustomVariableEnv("otel.otlp_timeout", "OTEL_EXPORTER_OTLP_TIMEOUT");

	/*
	 * https://docs.opentelemetry.io/reference/specification/sdk-environment-variables/#general-sdk-configuration
	 */
	{
		/* "OTEL_SDK_DISABLED=true" should no-op all telemetry signals */
		char *value = getenv("OTEL_SDK_DISABLED");
		if (value != NULL && pg_strcasecmp(value, "true") == 0)
			SetConfigOption("otel.export", "", PGC_POSTMASTER, PGC_S_ENV_VAR);
	}
	otel_CustomVariableEnv("otel.resource_attributes", "OTEL_RESOURCE_ATTRIBUTES");
	otel_CustomVariableEnv("otel.service_name", "OTEL_SERVICE_NAME");
}
