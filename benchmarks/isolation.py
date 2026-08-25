"""Subprocess isolation shared by bench_robustness.py and bench_memory.py.

The other benchmarks call every engine in-process, which is fine for
well-formed corpora but fatal for these two: a segfault in a C engine kills
the whole run, and a peak-RSS number is meaningless once four engines have
allocated in the same address space. So each measurement runs the calling
script again in a fresh interpreter (``python <script> worker <args...>``),
and the parent classifies how the child came back.

Protocol, from the worker side:

- ``stage(name)`` prints an ``@stage`` marker before starting a phase, so a
  crash or timeout is attributed to the phase that was running even though
  the process died without reporting.
- ``finish(payload)`` prints the final result as an ``@result`` JSON line.
  A worker that exits 0 without one is classified as a crash — that is what
  a swallowed ``SystemExit(0)`` looks like from outside.

Markers are prefix-tagged lines and the child runs with ``-u``, so engine
noise on stdout cannot be mistaken for a result and a marker cannot be lost
in a buffer when the process dies.
"""

from __future__ import annotations

import json
import signal
import subprocess
import sys

STAGE_PREFIX = "@stage "
RESULT_PREFIX = "@result "


def stage(name: str) -> None:
    """Mark, from inside a worker, the phase about to run."""
    print(STAGE_PREFIX + name, flush=True)


def finish(payload: dict[str, object]) -> None:
    """Report, from inside a worker, the final result."""
    print(RESULT_PREFIX + json.dumps(payload), flush=True)


def last_marker(out: str, prefix: str) -> str | None:
    marks = [line[len(prefix):] for line in out.splitlines() if line.startswith(prefix)]
    if not marks:
        return None
    return marks[-1]


def exit_detail(returncode: int) -> str:
    if returncode >= 0:
        return f"exit {returncode}"
    try:
        return signal.Signals(-returncode).name
    except ValueError:
        return f"signal {-returncode}"


def run_worker(script: str, args: list[str], timeout: float) -> dict[str, object]:
    """Run ``python script worker *args`` fresh and classify how it came back.

    Returns ``{"outcome", "stage", "payload", "detail"}`` where outcome is:

    - ``ok`` — the worker reported a result and exited 0; ``payload`` holds it
    - ``crash`` — signal, nonzero exit, or exit 0 without a result
    - ``timeout`` — still running after `timeout` seconds; SIGKILLed

    ``stage`` is the last ``@stage`` marker the child flushed, or None.
    A worker's own clean-exception classification travels inside ``payload``;
    this function only distinguishes processes that came back from processes
    that did not.
    """
    cmd = [sys.executable, "-u", script, "worker", *args]
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    try:
        out, _ = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        out, _ = proc.communicate()
        return {
            "outcome": "timeout",
            "stage": last_marker(out or "", STAGE_PREFIX),
            "payload": None,
            "detail": f"SIGKILL after {timeout:g}s",
        }
    mark = last_marker(out, STAGE_PREFIX)
    raw = last_marker(out, RESULT_PREFIX)
    if proc.returncode != 0:
        return {
            "outcome": "crash",
            "stage": mark,
            "payload": None,
            "detail": exit_detail(proc.returncode),
        }
    if raw is None:
        return {
            "outcome": "crash",
            "stage": mark,
            "payload": None,
            "detail": "exit 0 without a result",
        }
    try:
        payload = json.loads(raw)
    except ValueError:
        return {
            "outcome": "crash",
            "stage": mark,
            "payload": None,
            "detail": "unparseable result",
        }
    return {"outcome": "ok", "stage": mark, "payload": payload, "detail": ""}
