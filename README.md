# pg_otel

A loadable module for PostgreSQL that exports telemetry (logs and traces)
to an OpenTelemetry [collector][].

[collector]: https://opentelemetry.io/docs/collector/


## Getting Started

Add `pg_otel` to your [shared_preload_libraries][] and restart PostgreSQL.
Its default settings do nothing, so this is safe to leave in all the time.

```ini
# postgresql.conf
shared_preload_libraries = pg_otel
```

OpenTelemetry recommends running its [collector][] as an [agent](https://opentelemetry.io/docs/collector/deploy/)
on the same machine as the application generating telemetry data.
If that is how your system is arranged, this module's default settings are for you!

If your collector is somewhere else, you need to put its URL in `otel.otlp_endpoint`.

```sql
ALTER SYSTEM SET otel.otlp_endpoint TO 'https://my-collector:4318';
```

With that in place, the `otel.export_logs` and `otel.export_traces` settings start or stop the flow of telemetry.
These settings affect every connection to PostgreSQL, so the server needs to [reload][] to finally apply them.

```sql
ALTER SYSTEM SET otel.export_logs TO on;
ALTER SYSTEM SET otel.export_traces TO on;
SELECT pg_reload_conf();
```

You should see logs and traces in your collector and connected observability systems right away.
Congratulations! 🎉

If you want to stop exporting telemetry, [reset][] the settings to their defaults.

```sql
ALTER SYSTEM RESET otel.export_logs;
ALTER SYSTEM RESET otel.export_traces;
SELECT pg_reload_conf();
```

[reload]: https://www.postgresql.org/docs/current/functions-admin.html#FUNCTIONS-ADMIN-SIGNAL
[reset]: https://www.postgresql.org/docs/current/sql-altersystem.html
[shared_preload_libraries]: https://www.postgresql.org/docs/current/runtime-config-client.html#GUC-SHARED-PRELOAD-LIBRARIES


## Configuration

There are a number of other OpenTelemetry settings that can be adjusted for your environment.
Their respective [environment variables][sdk-env] also work.

```
+------------------------------+-----------------------+------+
|             name             |        default        | unit |
+------------------------------+-----------------------+------+
| otel.export_logs             | off                   |      | Controls whether error and log messages are exported
| otel.export_traces           | off                   |      | Controls whether statement execution traces are exported
| otel.otlp_batch_max_delay    | 1000                  | ms   | Max wait time before exporting a batch
| otel.otlp_batch_max_items    | 512                   |      | Max items per OTLP export batch
| otel.otlp_endpoint           | http://localhost:4318 |      | URL to which the exporter sends telemetry
| otel.otlp_headers            |                       |      | OTLP request headers (superuser only)
| otel.otlp_timeout            | 10000                 | ms   | Maximum time to deliver an export batch
| otel.resource_attributes     |                       |      | Instance metadata attached to all telemetry
| otel.service_name            | postgresql            |      | Logical name of this service
+------------------------------+-----------------------+------+
```

The following settings cannot be changed at this time:

```
+------------------------------+-----------------------+------+
|             name             |         value         | unit |
+------------------------------+-----------------------+------+
| otel.otlp_protocol           | http/protobuf         |      | The exporter transport protocol
+------------------------------+-----------------------+------+
```

[sdk-env]: https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/


## Development

`pg_otel` is built in Rust using [`pgrx`](https://github.com/pgcentralfoundation/pgrx).

### Testing

Run tests with `cargo pgrx test`:

```bash
# Test against a specific PostgreSQL version
cargo pgrx test pg15 -p pg_otel
cargo pgrx test pg16 -p pg_otel

# Run sequencing hook tests
cargo pgrx test pg16 -p sequencing
```

### Running Locally

```bash
# Launch an interactive psql session with pg_otel preloaded
cargo pgrx start   pg16 -p pg_otel --postgresql-conf 'shared_preload_libraries=pg_otel'
cargo pgrx connect pg16 -p pg_otel
cargo pgrx stop    pg16 -p pg_otel
```
