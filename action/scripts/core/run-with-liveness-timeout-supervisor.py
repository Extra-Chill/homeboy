#!/usr/bin/env python3
"""Linux subreaper supervisor with pidfd-based descendant containment."""

import argparse
import ctypes
import os
import signal
import subprocess
import sys
import time

PR_SET_CHILD_SUBREAPER = 36


def identity(pid):
    try:
        fields = open(f"/proc/{pid}/stat", encoding="utf-8").read().split()
        return None if fields[2] == "Z" else int(fields[21])
    except (FileNotFoundError, ProcessLookupError, IndexError, ValueError):
        return None


def descendants(root):
    children = {}
    for entry in os.scandir("/proc"):
        if not entry.name.isdigit():
            continue
        try:
            fields = open(f"/proc/{entry.name}/stat", encoding="utf-8").read().split()
            children.setdefault(int(fields[3]), []).append(int(entry.name))
        except (FileNotFoundError, ProcessLookupError, PermissionError, IndexError, ValueError):
            # A process that exits between scandir and read surfaces as ESRCH
            # (ProcessLookupError), not just ENOENT. Scanning all of /proc means
            # this race is routine; letting it escape kills the supervisor and
            # orphans the very descendants it exists to contain.
            continue
    result, pending = set(), [root]
    while pending:
        for child in children.get(pending.pop(), []):
            if child not in result:
                result.add(child)
                pending.append(child)
    return result


def open_pidfd(pid):
    try:
        return os.pidfd_open(pid, 0)
    except ProcessLookupError:
        return None


def signal_pidfd(pidfd, signal_number):
    try:
        signal.pidfd_send_signal(pidfd, signal_number)
        return True
    except ProcessLookupError:
        return True
    except PermissionError:
        return False


def known_live(known):
    try:
        return any(identity(pid) == start for pid, start in known)
    except PermissionError:
        return None


def reap_children():
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return


def close_pidfds(known):
    for pidfd in known.values():
        try:
            os.close(pidfd)
        except OSError:
            pass


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--log-file", default="")
    parser.add_argument("label")
    parser.add_argument("timeout", type=int)
    parser.add_argument("cleanup_timeout", type=int)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not args.command:
        return 2
    if sys.platform != "linux":
        print(f"::error::{args.label} requires Linux pidfd containment; refusing to run this strict command.")
        return 125
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        print(f"::error::{args.label} requires Linux pidfd containment; refusing to run this strict command.")
        return 125
    try:
        probe = open_pidfd(os.getpid())
        if probe is None:
            return 125
        os.close(probe)
    except PermissionError:
        print(f"::error::{args.label} lacks permission for Linux pidfd containment; refusing to run this strict command.")
        return 125
    if ctypes.CDLL(None).prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        print(f"::error::{args.label} cannot establish a Linux child subreaper; refusing to run this strict command.")
        return 125

    output = open(args.log_file, "wb") if args.log_file else None
    process = subprocess.Popen(args.command, stdout=output, stderr=subprocess.STDOUT if output else None, preexec_fn=os.setsid)
    known = {}
    root_start = identity(process.pid)
    try:
        root_pidfd = open_pidfd(process.pid) if root_start is not None else None
    except PermissionError:
        process.kill()
        process.wait()
        return 125
    if root_pidfd is not None:
        known[(process.pid, root_start)] = root_pidfd
    started, timed_out = time.monotonic(), False

    def observe():
        try:
            observed = descendants(os.getpid())
            for pid in observed:
                start = identity(pid)
                key = (pid, start)
                if start is not None and key not in known:
                    pidfd = open_pidfd(pid)
                    if pidfd is not None:
                        known[key] = pidfd
            return True
        except PermissionError:
            return False

    def terminate(reason):
        print(f"::warning::{args.label} {reason}; terminating supervised descendants by pidfd.")
        if not observe():
            return False
        for pidfd in known.values():
            if not signal_pidfd(pidfd, signal.SIGTERM):
                return False
        for signal_number, name in ((signal.SIGTERM, "TERM"), (signal.SIGKILL, "KILL")):
            deadline = time.monotonic() + args.cleanup_timeout
            while time.monotonic() < deadline:
                if not observe():
                    return False
                live = known_live(known)
                if live is False:
                    return True
                if live is None:
                    return False
                time.sleep(0.05)
            if signal_number == signal.SIGTERM:
                print(f"::warning::{args.label} containment grace expired; sending SIGKILL by pidfd.")
                for pidfd in known.values():
                    if not signal_pidfd(pidfd, signal.SIGKILL):
                        return False
        return False

    while process.poll() is None:
        if not observe():
            close_pidfds(known)
            return 125
        elapsed = int(time.monotonic() - started)
        if elapsed >= args.timeout:
            timed_out = True
            print(f"::error::{args.label} exceeded its {args.timeout}s execution timeout.")
            if not terminate("timed out"):
                close_pidfds(known)
                return 125
            break
        time.sleep(0.05)

    exit_code = process.wait()
    reap_children()
    if not observe() or known_live(known) is None:
        close_pidfds(known)
        return 125
    leaked_containment = False
    if known_live(known):
        leaked_containment = True
        if not terminate("finalization found live command containment"):
            close_pidfds(known)
            return 125
    reap_children()
    close_pidfds(known)
    if output:
        output.close()
    if timed_out:
        return 124
    # A command that outlived its own exit by leaking descendants we had to
    # force-terminate did not finish cleanly, whatever it reported. Strict mode
    # exists to prove containment, so refuse to launder that into a pass. A real
    # non-zero exit still wins: it is more actionable than a synthetic timeout.
    if leaked_containment and exit_code == 0:
        print(f"::error::{args.label} leaked descendants that outlived the command; reporting containment failure.")
        return 124
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
