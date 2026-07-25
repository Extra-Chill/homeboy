#!/usr/bin/env python3
"""Linux subreaper supervisor for commands that need descendant containment."""

import argparse
import ctypes
import os
import signal
import subprocess
import sys
import time


PR_SET_CHILD_SUBREAPER = 36


def identity(pid: int):
    try:
        fields = open(f"/proc/{pid}/stat", encoding="utf-8").read().split()
        return None if fields[2] == "Z" else int(fields[21])
    except (FileNotFoundError, ProcessLookupError, IndexError, ValueError):
        return None


def descendants(root: int):
    children = {}
    for entry in os.scandir("/proc"):
        if not entry.name.isdigit():
            continue
        try:
            fields = open(f"/proc/{entry.name}/stat", encoding="utf-8").read().split()
            children.setdefault(int(fields[3]), []).append(int(entry.name))
        except (FileNotFoundError, IndexError, ValueError):
            continue
    result, pending = set(), [root]
    while pending:
        parent = pending.pop()
        for child in children.get(parent, []):
            if child not in result:
                result.add(child)
                pending.append(child)
    return result


def signal_identity(pid, start, signal_number):
    """Signal only the observed process generation; ESRCH means it already exited."""
    try:
        if identity(pid) != start:
            return True
        os.kill(pid, signal_number)
        return True
    except ProcessLookupError:
        return True
    except PermissionError:
        return False


def identities_live(known):
    try:
        return any(identity(pid) == start for pid, start in known.items())
    except PermissionError:
        return None


def reap_children():
    """Reap all adopted descendants after the direct command has been reaped."""
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return


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
        print(f"::error::{args.label} requires Linux subreaper containment; refusing to run this strict command.")
        return 125
    if ctypes.CDLL(None).prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        print(f"::error::{args.label} cannot establish a Linux child subreaper; refusing to run this strict command.")
        return 125

    output = open(args.log_file, "wb") if args.log_file else None
    process = subprocess.Popen(args.command, stdout=output, stderr=subprocess.STDOUT if output else None, preexec_fn=os.setsid)
    root_start = identity(process.pid)
    known = {}
    started = time.monotonic()
    timed_out = False

    def observe():
        try:
            observed = descendants(os.getpid())
        except PermissionError:
            return False
        for pid in observed:
            try:
                start = identity(pid)
            except PermissionError:
                return False
            if start is not None:
                known[pid] = start
        return True

    def terminate(reason):
        print(f"::warning::{args.label} {reason}; terminating supervised descendants.")
        if not observe():
            return False
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except PermissionError:
            return False
        for pid, start in known.items():
            if not signal_identity(pid, start, signal.SIGTERM):
                return False
        deadline = time.monotonic() + args.cleanup_timeout
        while time.monotonic() < deadline:
            if not observe():
                return False
            live = identities_live(known)
            if live is False:
                return True
            if live is None:
                return False
            time.sleep(0.05)
        print(f"::warning::{args.label} containment grace expired; sending SIGKILL to supervised descendants.")
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except PermissionError:
            return False
        for pid, start in known.items():
            if not signal_identity(pid, start, signal.SIGKILL):
                return False
        deadline = time.monotonic() + args.cleanup_timeout
        while time.monotonic() < deadline:
            if not observe():
                return False
            live = identities_live(known)
            if live is False:
                return True
            if live is None:
                return False
            time.sleep(0.05)
        return False

    while process.poll() is None:
        if not observe():
            print(f"::error::{args.label} cannot observe supervised descendants.")
            return 125
        elapsed = int(time.monotonic() - started)
        if elapsed >= args.timeout:
            timed_out = True
            print(f"::error::{args.label} exceeded its {args.timeout}s execution timeout.")
            if not terminate("timed out"):
                print(f"::error::{args.label} containment could not prove cleanup. Retained command output: {args.log_file or 'standard output'}.")
                return 125
            break
        if elapsed and elapsed % 60 == 0:
            print(f"::notice::{args.label} is still running after {elapsed}s (timeout: {args.timeout}s).")
        time.sleep(0.05)

    exit_code = process.wait()
    reap_children()
    if not observe():
        print(f"::error::{args.label} cannot observe supervised descendants.")
        return 125
    live = identities_live(known)
    if live is None:
        print(f"::error::{args.label} cannot verify supervised descendant identities.")
        return 125
    if live:
        if not terminate("finalization found live command containment"):
            print(f"::error::{args.label} containment could not prove cleanup. Retained command output: {args.log_file or 'standard output'}.")
            return 125
        print(f"::notice::{args.label} finalization terminated surviving command containment; retained command output: {args.log_file or 'standard output'}.")
    reap_children()
    if output:
        output.close()
    if timed_out:
        print(f"::error::{args.label} exceeded its {args.timeout}s execution timeout and was terminated. Inspect this step's logs and rerun after resolving the blocked command; the caller can continue when this action is advisory.")
        return 124
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
