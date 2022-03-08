
use strict;
use warnings;

use PostgresNode;
use TestLib;
use Test::More;

my $node = PostgresNode->get_new_node('main');
$node->init();
$node->append_conf('postgresql.conf', qq(
shared_preload_libraries = pg_otel_logs
));
$node->start();

my $contents = slurp_file('/tmp/pg-otel.ndjson');
like($contents, qr/
	.+?\{"timeUnixNano":"\d+","severityNumber":"SEVERITY_NUMBER_INFO","severityText":"LOG","body":\{"stringValue":"starting\ PostgreSQL
	.+?\{"timeUnixNano":"\d+","severityNumber":"SEVERITY_NUMBER_INFO","severityText":"LOG","body":\{"stringValue":"listening
/sx, 'hook works for initial messages');

$node->psql('postgres', 'SELECT 1/0');
$contents = slurp_file('/tmp/pg-otel.ndjson', length($contents));
like($contents, qr/
	.+?\{"timeUnixNano":"\d+","severityNumber":"SEVERITY_NUMBER_ERROR","severityText":"ERROR","body":\{"stringValue":"division\ by\ zero"
/sx, 'error level');

$node->stop();
done_testing();
