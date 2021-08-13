#!/usr/bin/env bash
set -eu

mkdir -p /build
cp -R -t /build /mnt/*
cd /build

make clean all install

chown -R postgres: .

check() {
	make installcheck EXTRA_REGRESS_OPTS="--temp-instance=$(mktemp -d)" && exit

	local failed=$?

	if [ -s regression.diffs ]; then
		cat regression.diffs
	else
		cat log/postmaster.log
	fi

	cat /tmp/pg-otel.txt

	exit $failed
}

export -f check && su postgres -- -c check
