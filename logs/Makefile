MODULES = pg_otel_logs

REGRESS = test
REGRESS_OPTS = --temp-config='test/postgresql.conf'

PG_CONFIG = pg_config
PGXS := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)

docker-check:
	cd test && docker build --tag 'pg_otel_logs-test' .
	docker run --rm --volume "$$(pwd):/mnt" 'pg_otel_logs-test' /mnt/test/docker-check.sh
