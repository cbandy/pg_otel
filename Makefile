# https://git.postgresql.org/gitweb/?p=postgresql.git;a=blob;f=src/makefiles/pgxs.mk
# https://www.postgresql.org/docs/current/extend-pgxs.html

MODULE_big = pg_otel
OBJS = pg_otel.o $(OTEL_PROTO_FILES:.proto=.pb-c.o)

OTEL_PROTO_NEEDED = collector/logs common resource logs
OTEL_PROTO_FILES = $(patsubst opentelemetry-proto/%,%,\
	$(wildcard $(patsubst %,opentelemetry-proto/opentelemetry/proto/%/*/*.proto,$(OTEL_PROTO_NEEDED))))

REGRESS = config
REGRESS_OPTS = --temp-config='test/postgresql.conf'

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
	cp -R opentelemetry-proto/opentelemetry ./
	protoc --c_out=. $(OTEL_PROTO_FILES)

.PHONY: docker-check
docker-check:
	cd test && docker build --tag 'pg_otel-test' .
	docker run --rm -t -v "$$(pwd):/mnt" -w '/mnt' -u "$$(id -u)" 'pg_otel-test' make otel-protobufs
	docker run --rm -t -v "$$(pwd):/mnt" -w '/mnt' 'pg_otel-test' ./test/docker-check.sh
