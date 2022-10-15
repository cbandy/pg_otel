FROM docker.io/library/buildpack-deps

ARG PG_MAJOR=14

RUN . /etc/os-release && echo > /etc/apt/sources.list.d/pgdg.list \
 "deb http://apt.postgresql.org/pub/repos/apt/ ${VERSION_CODENAME}-pgdg main ${PG_MAJOR}"

RUN export APT_KEY_DONT_WARN_ON_DANGEROUS_USAGE=1 \
 && curl -sS 'https://www.postgresql.org/media/keys/ACCC4CF8.asc' | apt-key add -

RUN export DEBIAN_FRONTEND=noninteractive \
 && apt-get update \
 && apt-get install --no-install-recommends --yes cgreen1 libcgreen1-dev \
 && apt-get install --no-install-recommends --yes postgresql-common libipc-run-perl \
 && apt-get install --no-install-recommends --yes libprotobuf-dev protobuf-compiler \
 && apt-get install --no-install-recommends --yes libprotobuf-c-dev protobuf-c-compiler \
 && echo >> /etc/postgresql-common/createcluster.conf 'create_main_cluster = false' \
 && apt-get install --no-install-recommends --yes "postgresql-${PG_MAJOR}" "postgresql-server-dev-${PG_MAJOR}"

# https://opentelemetry.io/docs/collector/getting-started/#linux-packaging
RUN REPO='open-telemetry/opentelemetry-collector-releases' VERSION=0.61.0 \
 && wget "https://github.com/${REPO}/releases/download/v${VERSION}/otelcol_${VERSION}_linux_amd64.deb" \
 && dpkg --install "otelcol_${VERSION}_linux_amd64.deb"
