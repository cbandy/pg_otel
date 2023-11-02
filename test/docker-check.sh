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
		tail -vn+0 regression.diffs
	else
		tail -vn+0 log/postmaster.log
	fi

	tail -vn+0 tmp_check/log/*

	exit $failed
}

export -f check && su postgres -- -c check
