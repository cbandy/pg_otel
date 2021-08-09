# pg_otel_logs

A `shared_preload_libraries` that sets [emit_log_hook][] and runs a [background worker][]
that batches gRPC calls to an OpenTelemetry [collector][].

[background worker]: https://www.postgresql.org/docs/current/bgworker.html
[collector]: https://opentelemetry.io/docs/concepts/data-collection/
[emit_log_hook]: https://git.postgresql.org/gitweb/?p=postgresql.git;a=blob;f=src/backend/utils/error/elog.c


`_PG_init()`
- create pipe for IPC with worker
- register background worker
- set log hook

`_PG_fini()`
- unset log hook


### Worker IPC

Use the same pipe protocol as logger, [write_pipe_chunks][] and [syslogger.c][],
but write one JSON array per log message:

- protocol version number: `1`
- time, as nanoseconds since UNIX Epoch
- severity
- message

Consider which/how other fields fit the OpenTelemetry [data model][] and [semantic conventions][].

The worker will arrange values as LogRecord fields/attributes.

[data model]: https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/logs/data-model.md
[semantic conventions]: https://github.com/open-telemetry/opentelemetry-specification/tree/main/semantic_conventions
[syslogger.c]: https://git.postgresql.org/gitweb/?p=postgresql.git;a=blob;f=src/backend/postmaster/syslogger.c
[write_pipe_chunks]: https://git.postgresql.org/gitweb/?p=postgresql.git;a=blob;f=src/backend/utils/error/elog.c
