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
        return int(open(f"/proc/{pid}/stat", encoding="utf-8").read().split()[21])
    except (FileNotFoundError, IndexError, ValueError):
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


def live(pid, start):
    return identity(pid) == start


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
        for pid in descendants(os.getpid()):
            start = identity(pid)
            if start is not None:
                known[pid] = start

    def terminate(reason):
        print(f"::warning::{args.label} {reason}; terminating supervised descendants.")
        observe()
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        for pid, start in known.items():
            if live(pid, start):
                os.kill(pid, signal.SIGTERM)
        deadline = time.monotonic() + args.cleanup_timeout
        while time.monotonic() < deadline:
            observe()
            if not any(live(pid, start) for pid, start in known.items()):
                return True
            time.sleep(0.05)
        print(f"::warning::{args.label} containment grace expired; sending SIGKILL to supervised descendants.")
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        for pid, start in known.items():
            if live(pid, start):
                os.kill(pid, signal.SIGKILL)
        deadline = time.monotonic() + args.cleanup_timeout
        while time.monotonic() < deadline:
            observe()
            if not any(live(pid, start) for pid, start in known.items()):
                return True
            time.sleep(0.05)
        return False

    while process.poll() is None:
        observe()
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
    observe()
    if any(live(pid, start) for pid, start in known.items()):
        if not terminate("finalization found live command containment"):
            print(f"::error::{args.label} containment could not prove cleanup. Retained command output: {args.log_file or 'standard output'}.")
            return 125
        print(f"::notice::{args.label} finalization terminated surviving command containment; retained command output: {args.log_file or 'standard output'}.")
    if output:
        output.close()
    if timed_out:
        print(f"::error::{args.label} exceeded its {args.timeout}s execution timeout and was terminated. Inspect this step's logs and rerun after resolving the blocked command; the caller can continue when this action is advisory.")
        return 124
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
