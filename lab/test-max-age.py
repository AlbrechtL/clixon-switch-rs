#!/usr/bin/env python3
"""Max age test on the chain r1 - s1 - ... - sN - r2 (lab/chain.yaml).

After https://vincent.bernat.ch/en/blog/2026-spanning-tree: a BPDU's
message age grows by one per bridge, and a bridge discards BPDUs whose
message age has reached the max age. With the default max age of 20, r1's
BPDUs reach s1..s20 only, and s21..sN elect r2, the next best bridge. The
root's max age is what counts: every bridge uses the timers in the root's
BPDUs.

    1. Default max age 20: r1, s1..s20 have root r1; s21..sN, r2 root r2.
    2. max-age 40, forwarding-delay 21 on r1 only
       (2 * (forwarding-delay - 1) >= max-age): every bridge has root r1.
    3. Back to 20 and 15 on r1: the chain splits again at s20 | s21.

The chain has no loop, so in every phase every port forwards.

    lab/up.sh -f lab/chain.yaml
    lab/test-max-age.py
    docker compose -f lab/chain.yaml down
"""

import argparse
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
os.environ.setdefault("LAB_COMPOSE", os.path.join(HERE, "chain.yaml"))
sys.path.insert(0, HERE)
import labctl  # noqa: E402

RSTP_CONFIG = "openconfig-spanning-tree:stp/rstp/config"


def chain_nodes():
    """r1, s1..sN, r2 in chain order, from the running lab."""
    names = labctl.nodes()
    middle = sorted((n for n in names if n.startswith("s")), key=lambda n: int(n[1:]))
    if "r1" not in names or "r2" not in names or not middle:
        sys.exit(f"not the chain lab: {names} (lab/up.sh -f lab/chain.yaml)")
    return ["r1", *middle, "r2"]


def expected_roots(chain, max_age):
    """r1's BPDUs reach the bridges at most max_age hops away; r2 is root
    of the rest."""
    return {n: "r1" if i <= max_age else "r2" for i, n in enumerate(chain)}


def problems(chain, states, want):
    if any(st is None for st in states.values()):
        off = [n for n, st in states.items() if st is None]
        return [f"no spanning tree state on {', '.join(off)}"
                " (Linux < 7.1 has no bridge stp_mode?)"]
    roots = labctl.root_names(states)
    out = [f"{n}: root {roots[n]}, expected {want[n]}" for n in chain if roots[n] != want[n]]
    out += [f"{n}:{p} {ps['state']}" for n in chain
            for p, ps in states[n]["ports"].items() if ps["state"] != "FORWARDING"]
    return out


def summary(chain, states):
    """r1 x21 | r2 x17: the roots along the chain, run-length encoded."""
    roots = labctl.root_names(states)
    runs = []
    for n in chain:
        r = roots[n] or "off"
        if runs and runs[-1][0] == r:
            runs[-1][1] += 1
        else:
            runs.append([r, 1])
    return " | ".join(f"{r} x{c}" for r, c in runs)


def wait_for(chain, want, timeout):
    deadline = time.monotonic() + timeout
    start = time.monotonic()
    while True:
        states = labctl.stp_states(chain)
        found = problems(chain, states, want)
        if not found:
            print(f"  converged after {time.monotonic() - start:.0f} s: {summary(chain, states)}")
            return True
        if time.monotonic() > deadline:
            print(f"  not converged after {timeout:.0f} s: {summary(chain, states)}")
            for p in found[:20]:
                print(f"  FAIL: {p}")
            return False
        time.sleep(1)


def set_timers(node, max_age, forwarding_delay):
    body = json.dumps({"openconfig-spanning-tree:config":
                       {"max-age": max_age, "forwarding-delay": forwarding_delay}})
    status, data = labctl.restconf(node, "PATCH", RSTP_CONFIG, body)
    if status >= 300:
        sys.exit(f"{node}: PATCH {RSTP_CONFIG}: HTTP {status} {json.dumps(data)}")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--timeout", type=float, default=90,
                    help="seconds to wait for each phase to converge")
    args = ap.parse_args()

    chain = chain_nodes()
    print(f"chain of {len(chain)} bridges: {chain[0]} - {chain[1]} ... {chain[-2]} - {chain[-1]}")
    if len(chain) - 1 <= 20:
        sys.exit("the chain is too short to split at max age 20")

    phases = [
        ("1. max age 20 (default): the chain splits after s20", None, 20),
        ("2. max age 40, forwarding delay 21 on r1: one tree", (40, 21), 40),
        ("3. max age 20, forwarding delay 15 on r1 again: split again", (20, 15), 20),
    ]
    for title, timers, max_age in phases:
        print(title)
        if timers:
            set_timers("r1", *timers)
        # Each phase starts from the one before: stop at the first failure.
        if not wait_for(chain, expected_roots(chain, max_age), args.timeout):
            sys.exit("FAILED")
    print("OK")


if __name__ == "__main__":
    main()
