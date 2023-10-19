/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

#include "postgres.h"
#include "parser/scansup.h"
#include "utils/guc.h"
#include "utils/varlena.h"

#include "curl/curl.h"

#include "pg_otel_config.h"

#if PG_VERSION_NUM < 160000
/* https://git.postgresql.org/gitweb/?p=postgresql.git;h=0a20ff54f5e661589 */
/* https://git.postgresql.org/gitweb/?p=postgresql.git;h=407b50f2d421bca5b */
#include "utils/elog.h"
#define guc_free free
static void *guc_malloc(int elevel, size_t size)
{
	void *data = malloc(size);
	if (data == NULL)
		ereport(elevel,
				(errcode(ERRCODE_OUT_OF_MEMORY),
				 errmsg("out of memory")));
	return data;
}
static char *guc_strdup(int elevel, const char *src)
{
	char *data = strdup(src);
	if (data == NULL)
		ereport(elevel,
				(errcode(ERRCODE_OUT_OF_MEMORY),
				 errmsg("out of memory")));
	return data;
}
#endif

/*
 * otel_ScanW3CBaggage parses a W3C Baggage key or value that begins at start.
 * It returns false when the string has an invalid character. When it returns
 * true, three optional arguments are populated with more details:
 *
 *   1. end contains a pointer just passed the end of the key or value.
 *   2. next contains a pointer to the following key or value.
 *   3. delimiter contains the delimiter found between them.
 *
 * See: https://www.w3.org/TR/baggage/
 */
static bool
otel_ScanW3CBaggage(const char state, const char *start,
					const char **end, const char **next, char *delimiter)
{
	const char KEY = ',', VALUE = '=';
	const char *pos = start;

	Assert(pos != NULL);

	while (*pos != '\0')
	{
		/*
		 * baggage-string   =  list-member 0*179( OWS "," OWS list-member )
		 * list-member      =  key OWS "=" OWS value *( OWS ";" OWS property )
		 * property         =  key OWS "=" OWS value
		 * property         =/ key OWS
		 */
		if (*pos == ',' || *pos == ';' || scanner_isspace(*pos))
			break;

		if (state != VALUE && *pos == '=')
			break;

		/*
		 * value            =  *baggage-octet
		 * baggage-octet    =  %x21 / %x23-2B / %x2D-3A / %x3C-5B / %x5D-7E
		 *
		 * This is more generous than the official specification for a baggage
		 * key but covers the delimiters we parse.
		 */
		if (*pos < '\x21' || *pos == '\x22' || *pos == '\x2C' ||
			*pos == '\x3B' || *pos == '\x5C' || *pos > '\x7E')
			return false;

		pos++;
	}

	/*
	 * key      =  token
	 * token    = 1*tchar
	 *
	 * Key cannot be empty.
	 */
	if (state == KEY && pos == start)
		return false;

	if (end != NULL)
		*end = pos;

	/* Optional whitespace */
	while (scanner_isspace(*pos))
		pos++;

	/* Delimiter or end of input */
	if (*pos != ',' && *pos != ';' && *pos != '=' && *pos != '\0')
		return false;

	if (delimiter != NULL)
		*delimiter = *pos;

	/* Step over the delimiter */
	if (*pos != '\0')
		pos++;

	/* Optional whitespace */
	while (scanner_isspace(*pos))
		pos++;

	if (next != NULL)
		*next = pos;

	return true;
}

static bool
otel_CheckW3CBaggage(const char *baggage)
{
	const char KEY = ',', VALUE = '=', PROPERTY = ';';
	const char *pos = baggage;
	char state = KEY;

	Assert(pos != NULL);

	/* Baggage should not start with whitespace, but allow it anyway */
	while (scanner_isspace(*pos))
		pos++;

	/* Allow empty string */
	if (*pos == '\0')
		return true;

	while (*pos != '\0')
	{
		char found;
		const char *start = pos, *end;

		/* Step passed one token */
		if (!otel_ScanW3CBaggage(state, pos, &end, &pos, &found))
			break;

		/* Validate any percent-encoding in a non-empty value */
		if (state == VALUE && start != end)
		{
			char *decoded = curl_easy_unescape(NULL, start, end - start, NULL);
			if (decoded == NULL)
				break;
			curl_free(decoded);
		}

		if (state == KEY && found == '=') state = VALUE;
		else if (state == VALUE && found == ',') state = KEY;
		else if (state == VALUE && found == ';') state = PROPERTY;
		else if (state == VALUE && found == '\0') return true;
		else if (state == PROPERTY && found == ',') state = KEY;
		else if (state == PROPERTY && found == ';') state = PROPERTY;
		else if (state == PROPERTY && found == '=') state = VALUE;
		else if (state == PROPERTY && found == '\0') return true;
		else
			break;
	}

	return false;
}

static bool
otel_CheckResourceAttributes(char **next, void **extra, GucSource source)
{
	const char KEY = ',', VALUE = '=', PROPERTY = ';';
	const char *read = *next;
	char state = KEY;
	char *write;
	size_t size;

	if (!otel_CheckW3CBaggage(*next))
	{
		GUC_check_errdetail("baggage syntax is invalid.");
		return false;
	}

	Assert(read != NULL);

	/* Ignore leading whitespace */
	while (scanner_isspace(*read))
		read++;

	/*
	 * Allocate enough for every key/value pair plus two null bytes.
	 * This will be freed by PostgreSQL GUC.
	 */
	size = strlen(read) + 2;
	*extra = write = guc_malloc(ERROR, size);
	MemSet(write, 0, size);

	while (*read != '\0')
	{
		if (state == KEY)
		{
			const char *start, *end;

			/* Read the key */
			start = read;
			otel_ScanW3CBaggage(state, read, &end, &read, &state);

			/* Copy it with a terminal null */
			memcpy(write, start, end - start);
			write += end - start + 1;

			Assert(state == VALUE);

			/* Read the value */
			start = read;
			otel_ScanW3CBaggage(state, read, &end, &read, &state);

			/* Decode and copy a non-empty value with a terminal null */
			if (start != end)
			{
				char *decoded = curl_easy_unescape(NULL, start, end - start, NULL);
				write = strcpy(write, decoded) + strlen(decoded) + 1;
				curl_free(decoded);
			}
			else
				write++;
		}
		else if (state == PROPERTY)
		{
			/* Step passed the property */
			otel_ScanW3CBaggage(state, read, NULL, &read, &state);

			/* Step passed the optional value */
			if (state == VALUE)
				otel_ScanW3CBaggage(state, read, NULL, &read, &state);
		}
		else
			pg_unreachable();
	}

	return true;
}

static void
otel_AssignResourceAttributes(const char *next, void *extra)
{
	config.resourceAttributes.parsed = (char *) extra;
}

static bool
otel_CheckEndpoint(char **next, void **extra, GucSource source)
{
	const char *const *protocol;
	curl_version_info_data *version = curl_version_info(CURLVERSION_NOW);

	if (strncmp(*next, "http://", 7) == 0)
	{
		for (protocol = version->protocols; protocol; protocol++)
			if (pg_strcasecmp(*protocol, "http") == 0)
				break;

		if (protocol == NULL)
		{
			GUC_check_errdetail("libcurl %s not compiled with support for HTTP.",
								version->version);
			return false;
		}
	}
	else if (strncmp(*next, "https://", 8) == 0)
	{
		for (protocol = version->protocols; protocol; protocol++)
			if (pg_strcasecmp(*protocol, "https") == 0)
				break;

		if (protocol == NULL)
		{
			GUC_check_errdetail("libcurl %s not compiled with support for HTTPS.",
								version->version);
			return false;
		}
	}
	else
	{
		GUC_check_errdetail("URL must begin with http or https.");
		return false;
	}

	/*
	 * libcurl 7.62.0 has functions for building and parsing URLs.
	 * - https://curl.se/libcurl/c/curl_url.html
	 */

	return true;
}

static bool
otel_CheckExports(char **next, void **extra, GucSource source)
{
	struct otelSignalConfiguration parsed = {};

	ListCell *cell = NULL;
	List     *list = NULL;

	parsed.text = guc_strdup(ERROR, *next);
	if (!SplitIdentifierString(parsed.text, ',', &list))
	{
		GUC_check_errdetail("List syntax is invalid.");
		guc_free(parsed.text);
		list_free(list);
		return false;
	}

	foreach(cell, list)
	{
		char *item = (char *) lfirst(cell);

		if (pg_strcasecmp(item, "logs") == 0 ||
			pg_strcasecmp(item, "log") == 0)
			parsed.signals |= PG_OTEL_CONFIG_LOGS;
		else if (pg_strcasecmp(item, "traces") == 0 ||
				 pg_strcasecmp(item, "trace") == 0 ||
				 pg_strcasecmp(item, "spans") == 0 ||
				 pg_strcasecmp(item, "span") == 0)
			parsed.signals |= PG_OTEL_CONFIG_TRACES;
		else
		{
			GUC_check_errdetail("Unrecognized signal: \"%s\".", item);
			guc_free(parsed.text);
			list_free(list);
			return false;
		}
	}

	guc_free(parsed.text);
	parsed.text = NULL;
	list_free(list);

	/* This will be freed by PostgreSQL GUC */
	*extra = guc_malloc(ERROR, sizeof(parsed));
	memcpy(*extra, &parsed, sizeof(parsed));
	return true;
}

static void
otel_AssignExports(const char *next, void *extra)
{
	struct otelSignalConfiguration *parsed =
		(struct otelSignalConfiguration *) extra;

	config.exports.signals = parsed->signals;
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
	 * https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/#parsing-empty-value
	 *
	 * > The SDK MUST interpret an empty value of an environment variable
	 * > the same way as when the variable is unset.
	 */
	if (value != NULL && value[0] != '\0')
		SetConfigOption(opt, value, PGC_POSTMASTER, PGC_S_ENV_VAR);
}

static void
otel_DefineCustomVariables()
{
	DefineCustomIntVariable
		("otel.attribute_count_limit",
		 "Maximum attributes allowed on each signal",
		 NULL,

		 &config.attributeCountLimit,
		 PG_OTEL_RESOURCE_MAX_ATTRIBUTES,
		 PG_OTEL_RESOURCE_MAX_ATTRIBUTES,
		 PG_OTEL_RESOURCE_MAX_ATTRIBUTES,

		 PGC_INTERNAL, 0, NULL, NULL, NULL);

	DefineCustomStringVariable
		("otel.export",
		 "Signals to export over OTLP",
		 "May be empty or \"logs\".",

		 &config.exports.text,
		 "",

		 PGC_SIGHUP, GUC_LIST_INPUT,
		 otel_CheckExports, otel_AssignExports, NULL);

	DefineCustomStringVariable
		("otel.otlp_endpoint",
		 "Target URL to which the exporter sends signals",

		 "A scheme of https indicates a secure connection."
		 " The per-signal endpoint configuration options take precedence.",

		 &config.otlp.endpoint,
		 "http://localhost:4318",

		 PGC_SIGHUP, 0, otel_CheckEndpoint, NULL, NULL);

	DefineCustomStringVariable
		("otel.otlp_protocol",
		 "The exporter transport protocol",
		 NULL,

		 &config.otlp.protocol,
		 "http/protobuf",

		 PGC_INTERNAL, 0, NULL, NULL, NULL);

	DefineCustomIntVariable
		("otel.otlp_timeout",
		 "Maximum time the exporter will wait for each batch export",
		 NULL,

		 &config.otlp.timeoutMS,
		 10 * 1000L, 1, 60 * 60 * 1000L, /* 10sec; between 1ms and 60min */

		 PGC_SIGHUP, GUC_UNIT_MS, NULL, NULL, NULL);

	DefineCustomStringVariable
		("otel.resource_attributes",
		 "Key-value pairs to be used as resource attributes",

		 "Formatted as W3C Baggage.",

		 &config.resourceAttributes.text,
		 "",

		 PGC_SIGHUP, 0,
		 otel_CheckResourceAttributes, otel_AssignResourceAttributes, NULL);

	DefineCustomStringVariable
		("otel.service_name",
		 "Logical name of this service",
		 NULL,

		 &config.serviceName,
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
	 * https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/#attribute-limits
	 */
	otel_CustomVariableEnv("otel.attribute_count_limit", "OTEL_ATTRIBUTE_COUNT_LIMIT");

	/*
	 * https://opentelemetry.io/docs/specs/otel/protocol/exporter/
	 */
	otel_CustomVariableEnv("otel.otlp_endpoint", "OTEL_EXPORTER_OTLP_ENDPOINT");
	otel_CustomVariableEnv("otel.otlp_protocol", "OTEL_EXPORTER_OTLP_PROTOCOL");
	otel_CustomVariableEnv("otel.otlp_timeout", "OTEL_EXPORTER_OTLP_TIMEOUT");

	/*
	 * https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/#general-sdk-configuration
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
