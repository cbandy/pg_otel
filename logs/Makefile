# https://git.postgresql.org/gitweb/?p=postgresql.git;a=blob;f=src/makefiles/pgxs.mk

MODULE_big = pg_otel_logs
OBJS = pg_otel_logs.o $(OTEL_PROTO_FILES:.proto=.pb-c.o)

OTEL_PROTO_NEEDED = collector/logs common resource logs
OTEL_PROTO_FILES = $(wildcard $(patsubst %,opentelemetry/proto/%/*/*.proto,$(OTEL_PROTO_NEEDED)))

#REGRESS = test
#REGRESS_OPTS = --temp-config='test/postgresql.conf'
TAP_TESTS = 1

PG_CONFIG = pg_config
PGXS := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)

CURL_CONFIG = curl-config
CFLAGS += $(shell $(CURL_CONFIG) --cflags)
SHLIB_LINK += $(shell $(CURL_CONFIG) --libs)
SHLIB_LINK += -lprotobuf-c

.PHONY: otel-protobufs
otel-protobufs:
	[ ! -d opentelemetry ] || rm -r opentelemetry
	cp -R ../opentelemetry-proto/opentelemetry ./
	protoc --c_out=. $(OTEL_PROTO_FILES)

.PHONY: docker-check
docker-check:
	cd test && docker build --tag 'pg_otel_logs-test' .
	@#
	[ ! -d opentelemetry ] || rm -r opentelemetry
	cp -R ../opentelemetry-proto/opentelemetry ./
	@#
	docker run --rm -v "$$(pwd):/mnt" -u "$$(id -u)" -w '/mnt' 'pg_otel_logs-test' \
	protoc --c_out=. $(OTEL_PROTO_FILES)
	@#
	docker run --rm -v "$$(pwd):/mnt" 'pg_otel_logs-test' /mnt/test/docker-check.sh
