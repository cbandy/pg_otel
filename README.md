# pg_otel

A loadable module for PostgreSQL that exports traces and server log messages
to an OpenTelemetry [collector][].

[collector]: https://opentelemetry.io/docs/collector/


## Getting Started

Add `pg_otel` to your [shared_preload_libraries][] and restart PostgreSQL.
Its default settings do nothing, so this is safe to leave in all the time.

```ini
# postgresql.conf
shared_preload_libraries = pg_otel
```

OpenTelemetry recommends running its [collector][] as an [agent][] on the same
machine as the application generating telemetry data. If that is how your
system is arranged, this module's default settings are for you!

If your collector is somewhere else, you need put its URL in `otel.otlp_endpoint`.

```sql
ALTER SYSTEM SET otel.otlp_endpoint TO 'https://my-collector:4318';
```

With that in place, the `otel.export` setting starts or stops the flow of logs.
These settings affect every connection to PostgreSQL, so the server needs to
[reload][] to finally apply them.

```sql
ALTER SYSTEM SET otel.export TO 'logs, traces';
SELECT pg_reload_conf();
```

You should see logs in your collector and connected logging systems right away.
Congratulations! 🎉

If you want to stop exporting logs, [reset][] `otel.export` to its empty default.

```sql
ALTER SYSTEM RESET otel.export;
SELECT pg_reload_conf();
```

[agent]: https://opentelemetry.io/docs/collector/deployment/
[reload]: https://www.postgresql.org/docs/current/functions-admin.html#FUNCTIONS-ADMIN-SIGNAL
[reset]: https://www.postgresql.org/docs/current/sql-altersystem.html
[shared_preload_libraries]: https://www.postgresql.org/docs/current/runtime-config-client.html#GUC-SHARED-PRELOAD-LIBRARIES


## Configuration

There are number of other OpenTelemetry settings that can be adjusted for your
environment. Their respective [environment variables][sdk-env] also work.

```
            name            |        default        | unit |                       description
----------------------------+-----------------------+------+-----------------------------------------------------------
 otel.export                |                       |      | Signals to export over OTLP
 otel.otlp_endpoint         | http://localhost:4318 |      | Target URL to which the exporter sends signals
 otel.otlp_timeout          | 10000                 | ms   | Maximum time the exporter will wait for each batch export
 otel.resource_attributes   |                       |      | Key-value pairs to be used as resource attributes
 otel.service_name          | postgresql            |      | Logical name of this service
```

The following settings cannot be changed at this time:

```
            name            |         value         | unit |                       description
----------------------------+-----------------------+------+-----------------------------------------------------------
 otel.attribute_count_limit | 128                   |      | Maximum attributes allowed on each signal
 otel.otlp_protocol         | http/protobuf         |      | The exporter transport protocol
```

[sdk-env]: https://opentelemetry.io/docs/specs/otel/configuration/sdk-environment-variables/

