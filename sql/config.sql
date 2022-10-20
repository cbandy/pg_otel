-- vim: set expandtab shiftwidth=0 syntax=pgsql tabstop=2 :

-- TEST: custom parameters and their defaults
\pset format csv
SELECT name, setting, unit, context, vartype, min_val, max_val, enumvals
  FROM pg_catalog.pg_settings
 WHERE name LIKE 'otel.%';
\pset format aligned

-- TEST: endpoint requires scheme
ALTER SYSTEM SET otel.otlp_endpoint TO 'localhost:8080';

-- TEST: protocol cannot be changed
ALTER SYSTEM SET otel.otlp_protocol TO 'grpc';

-- TEST: service.name cannot be blank
ALTER SYSTEM SET otel.service_name TO '';
