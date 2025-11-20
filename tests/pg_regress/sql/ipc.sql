-- SPDX-License-Identifier: ISC
-- vim: set expandtab foldmethod=marker shiftwidth=0 syntax=pgsql tabstop=2 :
\set VERBOSITY default

-- HACK: This is a goofy way to see that the exporter worker can receive IPC messages from a server backend.
-- HACK: This only works when the Postgres server is started with logging_collector=on.
--
--  1. This client sends SQL that raises a warning.
--  2. The backend sends the event over IPC to the worker.
--  3. The worker logs some info about the event.
--  4. This client reads from the CSV-formatted log.
--
-- TODO: Replace this test with an OTLP receiver for (3) and (4) above.

-- {{{ ✀  replace this with an OTLP endpoint
ALTER SYSTEM SET log_destination TO csvlog;
ALTER SYSTEM SET log_directory TO pg_otel_regress;
ALTER SYSTEM SET log_filename TO ipc;
ALTER SYSTEM SET log_min_messages TO info;
ALTER SYSTEM SET log_rotation_age TO 0;
ALTER SYSTEM SET log_rotation_size TO 0;
ALTER SYSTEM SET log_truncate_on_rotation TO yes;
-- }}}
ALTER SYSTEM SET otel.export TO logs;
DO $$ BEGIN PERFORM pg_catalog.pg_reload_conf(); END $$;

-- {{{ ✀  replace this with an OTLP receiver
DROP TABLE IF EXISTS public.postgres_log CASCADE;
CREATE TABLE public.postgres_log (
  _ts timestamptz(3),
  user_name text, database_name text,
  process_id bigint, remote_address text,
  session_id text, session_line bigint,
  command_tag text, session_start timestamptz,
  vtxid text, txid bigint, severity text, sql_state text,
  message text, detail text, hint text,
  internal_query text, internal_query_pos bigint,
  context text, query text, query_pos bigint,
  file_pos text, application text,
  CONSTRAINT empty CHECK (false) NO INHERIT
);

DO $$ BEGIN

PERFORM * FROM pg_catalog.pg_settings WHERE "name" = 'server_version_num' AND "setting" >= '130000';
IF FOUND THEN
  ALTER TABLE public.postgres_log ADD COLUMN backend_type text;
END IF;

PERFORM * FROM pg_catalog.pg_settings WHERE "name" = 'server_version_num' AND "setting" >= '140000';
IF FOUND THEN
  ALTER TABLE public.postgres_log ADD COLUMN leader_pid bigint;
  ALTER TABLE public.postgres_log ADD COLUMN query_id bigint;
END IF;

END $$;

DROP EXTENSION IF EXISTS file_fdw CASCADE;
CREATE EXTENSION file_fdw WITH SCHEMA pg_catalog;
CREATE SERVER files FOREIGN DATA WRAPPER file_fdw;
CREATE FOREIGN TABLE public.postgres_log_file () INHERITS (public.postgres_log)
SERVER files OPTIONS (filename 'pg_otel_regress/ipc.csv', format 'csv', header 'false');

COPY (SELECT 1 WHERE FALSE) TO PROGRAM 'tee pg_otel_regress/ipc.csv';
-- }}}

DO $$ BEGIN
  RAISE WARNING USING message = 'one', detail = 'two', hint = 'three', errcode = '2201W';
END $$;

-- {{{ ✀  replace this with assertions on the receiver state
\pset format unaligned
SELECT backend_type, substring(message for 25) FROM public.postgres_log_file WHERE severity = 'NOTICE';
\pset format aligned
-- }}}

ALTER SYSTEM RESET ALL;
DO $$ BEGIN PERFORM pg_catalog.pg_reload_conf(); END $$;
