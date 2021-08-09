.DEFAULT_GOAL := pg_otel_logs_exporter
.SUFFIXES:

GO ?= go

.PHONY: clean
clean:
	rm -f opentelemetry-collector-builder
	rm -f pg_otel_logs_exporter

opentelemetry-collector-builder:
	GOBIN=$(CURDIR) $(GO) install github.com/open-telemetry/opentelemetry-collector-builder@v0.31.0

go.mod: opentelemetry-collector-builder builder.yaml
	./opentelemetry-collector-builder \
		--config='builder.yaml' \
		--description='$@' --name='$@' \
		--output-path='$(CURDIR)' \
		--skip-compilation \
		--version='0.0.0'

pg_otel_logs_exporter: go.mod *.go stdin/*.go
	$(GO) mod tidy
	$(GO) build -ldflags='-s -w' -trimpath -o '$@'

