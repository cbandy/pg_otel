#!/usr/bin/env bash
set -eu

mkdir -p /build
cp -R -t /build /mnt/*
cd /build

touch /tmp/otelcol.log /tmp/otel.ndjson
otelcol > /tmp/otelcol.log 2>&1 \
	--config 'test/otel-collector.yaml' \
	--set 'exporters.file.path=/tmp/otel.ndjson' \
	&

make clean all install

chown -R postgres: .

check() {
	make installcheck EXTRA_REGRESS_OPTS="--temp-instance=$(mktemp -d)" && exit

	local failed=$?

	if [ -s regression.diffs ]; then
		tail -vn+0 regression.diffs
	else
		tail -vn+0 log/postmaster.log
	fi

	tail -vn+0 tmp_check/log/*

	tail -vn+0 /tmp/otelcol.log /tmp/otel.ndjson

	exit $failed
}

export -f check && su postgres -- -c check
