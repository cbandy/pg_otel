/* vim: set noexpandtab autoindent cindent tabstop=4 shiftwidth=4 cinoptions="(0,t0": */

/* FIXME: to the bgworker */
#include <stdio.h>
#include <sys/time.h>

#include "postgres.h"
#include "fmgr.h"
#include "lib/stringinfo.h"
#include "utils/json.h"

/* Dynamically loadable module */
PG_MODULE_MAGIC;

/* Hooks called when the module is (un)loaded */
void _PG_init(void);
void _PG_fini(void);

/* Hooks overridden by this module */
static emit_log_hook_type next_emit_log_hook = NULL;


/* FIXME: to the bgworker */
static FILE *debug;

/* Convert elevel to according to OpenTelemetry Log Data Model */
static int
otel_severity_number(int elevel)
{
	/*
	 * > If the log record represents a non-erroneous event the "SeverityNumber"
	 * > field … may be set to any numeric value less than ERROR (numeric 17).
	 *
	 * > If "SeverityNumber" is present and has a value of ERROR (numeric 17)
	 * > or higher then it is an indication that the log record represents an
	 * > erroneous situation.
	 */
	switch (elevel)
	{
		case DEBUG1:
			/* elog.c: syslog_level = LOG_DEBUG */
			return 5; /* DEBUG */
		case DEBUG2:
			/* elog.c: syslog_level = LOG_DEBUG */
			return 4; /* TRACE4 */
		case DEBUG3:
			/* elog.c: syslog_level = LOG_DEBUG */
			return 3; /* TRACE3 */
		case DEBUG4:
			/* elog.c: syslog_level = LOG_DEBUG */
			return 2; /* TRACE2 */
		case DEBUG5:
			/* elog.c: syslog_level = LOG_DEBUG */
			return 1; /* TRACE1 */
		case LOG:
		case LOG_SERVER_ONLY:
		case INFO:
			/* elog.c: syslog_level = LOG_INFO */
			return 9; /* INFO */
		case NOTICE:
			/* elog.c: syslog_level = LOG_NOTICE */
			return 10; /* INFO2 */
		case WARNING:
		case WARNING_CLIENT_ONLY:
			/* elog.c: syslog_level = LOG_NOTICE */
			return 13; /* WARN */
		case ERROR:
			/* elog.c: syslog_level = LOG_WARNING */
			return 17; /* ERROR */
		case FATAL:
			/* elog.c: syslog_level = LOG_ERR */
			return 21; /* FATAL */
		case PANIC:
		default:
			/* elog.c: syslog_level = LOG_CRIT */
			return 22; /* FATAL2 */
	}
}

/* Convert elevel to a JSON string identical to error_severity() in elog.c */
static const char *
otel_severity_text(int elevel)
{
	const char *text;

	switch (elevel)
	{
		case DEBUG1:
		case DEBUG2:
		case DEBUG3:
		case DEBUG4:
		case DEBUG5:
			text = "\"DEBUG\"";
			break;
		case LOG:
		case LOG_SERVER_ONLY:
			text = "\"LOG\"";
			break;
		case INFO:
			text = "\"INFO\"";
			break;
		case NOTICE:
			text = "\"NOTICE\"";
			break;
		case WARNING:
		case WARNING_CLIENT_ONLY:
			text = "\"WARNING\"";
			break;
		case ERROR:
			text = "\"ERROR\"";
			break;
		case FATAL:
			text = "\"FATAL\"";
			break;
		case PANIC:
			text = "\"PANIC\"";
			break;
		default:
			text = "\"\"";
			break;
	}

	return text;
}

/*
 * Called when a log message is not supressed by log_min_messages.
 */
static void
otel_emit_log_hook(ErrorData *edata)
{
	StringInfoData line;
	struct timeval tv;
	unsigned long long epoch_nanosec;

	gettimeofday(&tv, NULL);
	epoch_nanosec = tv.tv_sec * 1000000000 + tv.tv_usec * 1000;

	initStringInfo(&line);
	appendStringInfo(&line, "1,[%llu,%s,%d,", epoch_nanosec,
					 otel_severity_text(edata->elevel),
					 otel_severity_number(edata->elevel));

	escape_json(&line, edata->message);

	appendStringInfoString(&line, "]\n");

	/* TODO: write_pipe_chunks() */
	fwrite(line.data, 1, line.len, debug);

	pfree(line.data);

	/* Call the next log processor */
	if (next_emit_log_hook)
		(*next_emit_log_hook) (edata);
}


/* Called when the module is loaded */
void
_PG_init(void)
{
	/* TODO: move to the bgworker */
	debug = fopen("/tmp/pg-otel.txt", "w");

	/* Install our log processor */
	next_emit_log_hook = emit_log_hook;
	emit_log_hook = otel_emit_log_hook;
}

/* Called when the module is unloaded */
void
_PG_fini(void)
{
	/* Uninstall our log processor */
	emit_log_hook = next_emit_log_hook;

	/* TODO: move to the bgworker */
	fclose(debug);
}
