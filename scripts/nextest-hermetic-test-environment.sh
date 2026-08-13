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

# Give the test a PRIVATE temp root rather than unsetting TMPDIR (#12345).
#
# Unsetting it did not isolate anything. It handed every test the shared system
# `/tmp` -- the least isolated directory on the machine -- and silently assumed
# that directory is executable.
#
# On a host that mounts /tmp noexec, a fixture that writes a mock executable
# into its temp dir cannot run it, and the failure mode is not an error. Bash's
# PATH search calls access(X_OK); Linux fails that on a noexec mount, so bash
# SKIPS the mock and invokes the real binary further down PATH. The test does
# not report a missing mock -- it quietly talks to the real tool and asserts
# against whatever that returns. `tests/release_asset_completeness_test.rs`
# failed all 7 of its cases that way, reporting a GitHub release lookup error
# for a tag that never existed.
#
# The root lives under this workspace's `target/`, resolved from this script's
# own location so it does not depend on the caller's working directory or on
# the shape of the command being run.
#
# `target/` is the right filesystem by construction: cargo builds test binaries
# there and executes them from there, so if a test can run at all, that
# filesystem can host an executable fixture. No probing and no assumption.
#
# One root per invocation. Cargo's runner is invoked per test binary, and under
# nextest's process-per-test execution per test, so this is strictly more
# isolation than the shared /tmp it replaces, not less.
hermetic_script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
hermetic_tmp_parent="$(CDPATH= cd -- "$hermetic_script_dir/.." && pwd)/target/.test-tmp"
if ! mkdir -p "$hermetic_tmp_parent"; then
    echo "hermetic test environment: cannot create the temp root at $hermetic_tmp_parent" >&2
    exit 1
fi
TMPDIR="$(mktemp -d "$hermetic_tmp_parent/run.XXXXXX")"
export TMPDIR
# All three spellings must name the same root. Code reading TMP or TEMP must not
# land somewhere else than code reading TMPDIR.
TMP="$TMPDIR"
TEMP="$TMPDIR"
export TMP TEMP

# Cargo's runner is the last parent guaranteed to receive cancellation when a
# test binary is interrupted. A Perl parent uses waitpid(WNOHANG), rather than
# a shell `wait` held open by inherited descriptors, then reaps the test group.
# Homeboy test daemons deliberately stay in this group instead of daemonizing.
#
# It also owns the temp root above: this script `exec`s into it, so an EXIT trap
# here would never fire, and an accumulating per-test directory under `target/`
# would be a disk leak. Set HOMEBOY_TEST_KEEP_TMPDIR=1 to retain it for
# debugging.
exec perl -MPOSIX=setsid,WNOHANG -e '
    my $child = fork();
    die "fork: $!\n" unless defined $child;
    if ($child == 0) {
        setsid() or die "setsid: $!\n";
        exec @ARGV or die "exec: $!\n";
    }
    my $discard_tmpdir = sub {
        return if $ENV{HOMEBOY_TEST_KEEP_TMPDIR};
        my $tmpdir = $ENV{TMPDIR};
        # Only ever remove the directory this script created.
        return unless defined $tmpdir && $tmpdir =~ m{/\.test-tmp/run\.[^/]+$};
        system("rm", "-rf", "--", $tmpdir);
    };
    my $cleanup = sub {
        # A clean test leaves no process in its private group. Avoid charging
        # nextest process-per-test execution a grace period in that case.
        return unless kill 0, -$child;
        kill "TERM", -$child;
        # Give actual descendants a bounded chance to exit cleanly before
        # enforcing the same process-group cleanup guarantee.
        if (kill 0, -$child) {
            select undef, undef, undef, 1;
            kill "KILL", -$child if kill 0, -$child;
        }
        waitpid $child, 0;
    };
    $SIG{HUP} = $SIG{INT} = $SIG{TERM} = sub {
        $cleanup->();
        # Only after the process group is gone: a surviving descendant could
        # still be writing into the temp root.
        $discard_tmpdir->();
        exit 143;
    };
    my $status;
    while (($status = waitpid $child, WNOHANG) == 0) { select undef, undef, undef, 0.05; }
    my $exit = $?;
    $cleanup->();
    $discard_tmpdir->();
    exit($exit >> 8);
' -- "$@"
