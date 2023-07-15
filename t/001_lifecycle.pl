
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
));
$node->start();

# Expect events from startup to be exported
my $otlp_json = slurp_file($otlp_file);
like($otlp_json, qr/
	.+?"severityText":"LOG","body":\{"stringValue":"starting\ PostgreSQL
	.+?"severityText":"LOG","body":\{"stringValue":"listening
/sx, 'works for initial messages');

# Stop PostgreSQL
$node->stop();

# Expect events from shutdown to be exported
# - The "shutting down" message is emitted by the checkpointer process.
# - The "database system is shut down" message is emitted by postmaster during
#   an on_proc_exit hook. [miscinit.c]
$otlp_json = slurp_file($otlp_file, length($otlp_json));
like($otlp_json, qr/
	.+?"severityText":"LOG","body":\{"stringValue":"shutting\ down"
/sx, 'works for final messages');

# Stop the OpenTelemetry Collector
$collector->kill_kill();

done_testing();
