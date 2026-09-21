"""Polling helpers for state the switch reaches a moment after a commit.

mstpd, udhcpc and snmpd are separate processes, so their effects land after
the RESTCONF call returns. These poll instead of sleeping, and on timeout
fall through to a plain assert so pytest prints the difference.
"""

import time

DEFAULT_TIMEOUT = 10.0
INTERVAL = 0.05


def until_equal(fn, expected, *, timeout: float = DEFAULT_TIMEOUT, what: str = ""):
    """Polls `fn` until it returns `expected`. Returns the value."""
    deadline = time.monotonic() + timeout
    actual = None
    attempts = 0
    while True:
        actual = fn()
        attempts += 1
        if actual == expected:
            return actual
        if time.monotonic() >= deadline:
            break
        time.sleep(INTERVAL)
    # A bare assert, so pytest's rewriting prints the diff.
    label = what or getattr(fn, "__doc__", None) or repr(fn)
    print(f"\n{label}: still wrong after {timeout}s ({attempts} attempts)")
    assert actual == expected
    return actual


def until(pred, *, timeout: float = DEFAULT_TIMEOUT, what: str):
    """Polls `pred` until it is true. Fails with `what` in the message."""
    deadline = time.monotonic() + timeout
    attempts = 0
    while True:
        if pred():
            return
        attempts += 1
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"timed out after {timeout}s ({attempts} attempts) waiting for: {what}"
            )
        time.sleep(INTERVAL)


def settles(fn, *, stable_for: float = 1.0, timeout: float = 30.0, what: str = "state"):
    """Polls `fn` until its value stops changing for `stable_for` seconds.

    For spanning tree, where the question is not "is it X" but "has it
    finished moving". Returns the settled value.
    """
    deadline = time.monotonic() + timeout
    previous = object()
    since = time.monotonic()
    while True:
        current = fn()
        now = time.monotonic()
        if current != previous:
            previous, since = current, now
        elif now - since >= stable_for:
            return current
        if now >= deadline:
            raise AssertionError(
                f"{what} still changing after {timeout}s; last value: {current!r}"
            )
        time.sleep(INTERVAL)
