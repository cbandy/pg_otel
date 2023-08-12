
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

# Start PostgreSQL with logs enabled
$node->init();
$node->append_conf('postgresql.conf', qq(
shared_preload_libraries = pg_otel

otel.export = logs
otel.otlp_endpoint = http://localhost:${otlp_port}
otel.resource_attributes = ' one=tw%=o, three=, four==five; nada; prop=v, six=Am%C3%A9lie '
));
$node->start();


# TEST: resource attributes as W3C Baggage
my $otlp_json = slurp_file($otlp_file);
like($otlp_json, qr/
	.+? "resource":\{"attributes":\[
	[^]]*? \{"key":"four","value":\{"stringValue":"=five"\}\}
	[^]]*? \{"key":"one","value":\{"stringValue":"tw%=o"\}\}
	[^]]*? \{"key":"six","value":\{"stringValue":"Am\xC3\xA9lie"\}\}
	[^]]*? \{"key":"three","value":\{"stringValue":""\}\}
	[^]]*? \]\}
/sx, 'exports resource attributes');


# Stop PostgreSQL
$node->stop();

# Stop the OpenTelemetry Collector
$collector->kill_kill();

done_testing();
