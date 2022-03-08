-- vim: set expandtab shiftwidth=0 syntax=pgsql tabstop=2 :

-- TODO why
CREATE EXTENSION file_fdw;
CREATE SERVER otel_lines FOREIGN DATA WRAPPER file_fdw;
CREATE FOREIGN TABLE otel_lines (line text)
       SERVER otel_lines OPTIONS (filename '/tmp/pg-otel.txt');