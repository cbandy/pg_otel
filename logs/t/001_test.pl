
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

my $contents = slurp_file('/tmp/pg-otel.txt');
like($contents, qr/
	\A
	^1,\[\d+,"LOG",9,"starting\ PostgreSQL[^\n]+\n
	^1,\[\d+,"LOG",9,"listening
/mx, 'hook works for initial messages');

$node->psql('postgres', 'SELECT 1/0');
$contents = slurp_file('/tmp/pg-otel.txt', length($contents));
like($contents, qr/
	^1,\[\d+,"ERROR",17,"division\ by\ zero"]\n
/mx, 'error level');

$node->stop();
done_testing();
