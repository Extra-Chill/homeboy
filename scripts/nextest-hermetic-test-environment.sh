#!/bin/sh
set -eu

# Cargo owns the shared target for compilation. Test binaries must not inherit
# that production routing because several hermetic fixtures resolve source
# binaries and paths from their own isolated workspace.
unset CARGO_TARGET_DIR

# CI itself runs through Homeboy and exports its runtime root. The test harness
# owns its runtime root, so inheriting the controller's makes fixture state land
# outside each test's isolated home.
unset HOMEBOY_RUNTIME_TMPDIR
unset TMPDIR TMP TEMP

# Cargo's runner is the last parent guaranteed to receive cancellation when a
# test binary is interrupted. A Perl parent uses waitpid(WNOHANG), rather than
# a shell `wait` held open by inherited descriptors, then reaps the test group.
# Homeboy test daemons deliberately stay in this group instead of daemonizing.
exec perl -MPOSIX=setsid,WNOHANG -e '
    my $child = fork();
    die "fork: $!\n" unless defined $child;
    if ($child == 0) {
        setsid() or die "setsid: $!\n";
        exec @ARGV or die "exec: $!\n";
    }
    my $cleanup = sub {
        kill "TERM", -$child;
        select undef, undef, undef, 1;
        kill "KILL", -$child;
        waitpid $child, 0;
    };
    $SIG{HUP} = $SIG{INT} = $SIG{TERM} = sub { $cleanup->(); exit 143; };
    my $status;
    while (($status = waitpid $child, WNOHANG) == 0) { select undef, undef, undef, 0.05; }
    my $exit = $?;
    $cleanup->();
    exit($exit >> 8);
' -- "$@"
