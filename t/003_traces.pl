
use strict;
use warnings;

use IPC::Run ();
use PostgreSQL::Test::Cluster;
use PostgreSQL::Test::Utils;
use Test::More;

my $node = PostgreSQL::Test::Cluster->new('main');
my $otlp_port = PostgreSQL::Test::Cluster::get_free_port();

# Start the OpenTelemetry Collector
my $otlp_file = $node->basedir() . '/otlp.ndjson';
my $collector;
eval
{
	{ open my $fh, '>', $otlp_file; close $fh; };
	$collector = IPC::Run::start(
	['otelcol', '--config', 'test/otel-collector.yaml',
		'--set', "exporters.file.path=${otlp_file}",
		'--set', "receivers.otlp.protocols.http.endpoint=localhost:${otlp_port}"],
	'2>', $node->basedir() . '/otelcol.log');
};
if ($@)
{
	plan skip_all => 'otelcol (OpenTelemetry Collector) is needed to run this test';
}

# Start PostgreSQL with traces enabled
$node->init();
$node->append_conf('postgresql.conf', qq(
shared_preload_libraries = pg_otel

otel.export = traces
otel.otlp_endpoint = http://localhost:${otlp_port}
));
$node->start();


my ($otlp_json, $otlp_json_offset);
my $timeout = IPC::Run::timer($PostgreSQL::Test::Utils::timeout_default / 10);

sub slurp_otlp_json
{
	$otlp_json = '';
	until ($timeout->is_expired or $otlp_json ne '')
	{
		$otlp_json = slurp_file($otlp_file, $otlp_json_offset);
	}
	$otlp_json_offset += length($otlp_json);
}

# TEST: Simple query
$node->safe_psql('postgres', 'SELECT 5');
slurp_otlp_json();
like($otlp_json, qr/
	.+? "spans":\[
	[^]]*? "traceId":"[0-9a-f]{32}"
	[^]]*? "spanId":"[0-9a-f]{16}"
	[^]]*? "attributes":\[
	[^]]*? \{"key":"db.operation","value":\{"stringValue":"SELECT"\}\}
	[^]]*? \{"key":"db.postgresql.rows","value":\{"intValue":"1"\}\}
	[^]]*? \]
	[^]]*? \]
/sx);

# TEST: Simple DDL
$node->safe_psql('postgres', 'CREATE TABLE t1 (id int)');
slurp_otlp_json();
like($otlp_json, qr/
	.+? "spans":\[
	[^]]*? "traceId":"[0-9a-f]{32}"
	[^]]*? "spanId":"[0-9a-f]{16}"
	[^]]*? "attributes":\[
	[^]]*? \{"key":"db.operation","value":\{"stringValue":"CREATE\ TABLE"\}\}
	[^]]*? \]
	[^]]*? \]
/sx);

# TEST: Simple DML
$node->safe_psql('postgres', 'INSERT INTO t1 VALUES (3), (7)');
slurp_otlp_json();
like($otlp_json, qr/
	.+? "spans":\[
	[^]]*? "traceId":"[0-9a-f]{32}"
	[^]]*? "spanId":"[0-9a-f]{16}"
	[^]]*? "attributes":\[
	[^]]*? \{"key":"db.operation","value":\{"stringValue":"INSERT"\}\}
	[^]]*? \{"key":"db.postgresql.rows","value":\{"intValue":"2"\}\}
	[^]]*? \]
	[^]]*? \]
/sx);

# TEST: Trace Context
$node->safe_psql('postgres', '
	SET otel.traceparent = \'00-12345678901234567890123456789012-f00dcafe99facade-01\';
	SET otel.tracestate = \'asdf\';
	SELECT 1
');
slurp_otlp_json();
like($otlp_json, qr/
	.+? "spans":\[
	[^]]*? "traceId":"12345678901234567890123456789012"
	[^]]*? "spanId":"[0-9a-f]{16}"
	[^]]*? "traceState":"asdf"
	[^]]*? "parentSpanId":"f00dcafe99facade"
	[^]]*? \]
/sx);


# Stop PostgreSQL
$node->stop();

# Stop the OpenTelemetry Collector
$collector->kill_kill();

done_testing();
