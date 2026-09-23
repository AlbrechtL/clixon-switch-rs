#!/usr/bin/env python3
"""Drives the switches of a lab started by lab/up.sh.

Every command runs inside a switch container through `docker compose exec`:
RESTCONF with curl on localhost:8080, or any other command. Nothing needs
the containers' addresses to be reachable from the host.

    labctl.py nodes
    labctl.py get sw1 openconfig-spanning-tree:stp
    labctl.py set sw3 openconfig-spanning-tree:stp/rstp/config '{"openconfig-spanning-tree:config":{"bridge-priority":0}}'
    labctl.py put sw3 <path> <json>
    labctl.py delete sw3 <path>
    labctl.py exec sw2 mstpctl showport br-lan
    labctl.py stp
    labctl.py check [--root sw1] [--blocked N] [--timeout 30]

LAB_COMPOSE names the compose file (default: compose.yaml next to this
script).
"""

import argparse
import concurrent.futures
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
COMPOSE = os.environ.get("LAB_COMPOSE", os.path.join(HERE, "compose.yaml"))
RESTCONF = "http://localhost:8080/restconf/data"
JSON = "application/yang-data+json"


def compose(*args, **kw):
    return subprocess.run(["docker", "compose", "-f", COMPOSE, *args], **kw)


def nodes():
    out = compose("ps", "--services", "--status", "running",
                  capture_output=True, text=True, check=True).stdout
    return sorted(out.split())


def exec_in(node, *cmd, check=True):
    return compose("exec", "-T", node, *cmd,
                   capture_output=True, text=True, check=check)


def restconf(node, method, path, body=None):
    """Returns (HTTP status, parsed JSON body or None)."""
    cmd = ["curl", "-s", "-X", method, "-H", f"Accept: {JSON}",
           "-w", "\n%{http_code}", f"{RESTCONF}/{path}"]
    if body is not None:
        cmd += ["-H", f"Content-Type: {JSON}", "-d", body]
    out = exec_in(node, *cmd).stdout
    text, _, status = out.rpartition("\n")
    return int(status), json.loads(text) if text.strip() else None


def identity(value):
    """oc-stp-types:BLOCKING -> BLOCKING"""
    return value.rsplit(":", 1)[-1] if isinstance(value, str) else value


def stp_state(node):
    """The spanning tree state of one node, or None when it runs none.

    Only STP and RSTP, which keep their state in stp/rstp."""
    status, data = restconf(node, "GET", "openconfig-spanning-tree:stp")
    if status != 200 or not data:
        return None
    stp = data.get("openconfig-spanning-tree:stp", {})
    rstp = stp.get("rstp", {})
    state = rstp.get("state", {})
    if "bridge-address" not in state:
        return None
    ports = {}
    for i in rstp.get("interfaces", {}).get("interface", []):
        s = i.get("state", {})
        ports[i["name"]] = {"role": identity(s.get("role")),
                            "state": identity(s.get("port-state"))}
    return {
        "protocol": [identity(p) for p in
                     stp.get("global", {}).get("state", {}).get("enabled-protocol", [])],
        "bridge": (state.get("bridge-priority"), state.get("bridge-address")),
        "root": (state.get("designated-root-priority"),
                 state.get("designated-root-address")),
        "root-port": state.get("root-port"),
        "ports": ports,
    }


def stp_states(names):
    """stp_state of every node, fetched in parallel: {node: state}."""
    with concurrent.futures.ThreadPoolExecutor(max_workers=16) as pool:
        return dict(zip(names, pool.map(stp_state, names)))


def links():
    """The links of the topology: networks other than default, with the
    services attached to them."""
    cfg = json.loads(compose("config", "--format", "json",
                             capture_output=True, text=True, check=True).stdout)
    attached = {}
    for name, svc in cfg.get("services", {}).items():
        for net in (svc.get("networks") or {}):
            if net != "default":
                attached.setdefault(net, []).append(name)
    return attached


def fmt_id(bid):
    return f"{bid[0]}/{bid[1]}" if bid and bid[1] else "-"


def root_names(states):
    """{node: name of the node it takes for the root, or its bridge ID}"""
    by_id = {st["bridge"]: n for n, st in states.items() if st}
    return {n: by_id.get(st["root"], fmt_id(st["root"])) if st else None
            for n, st in states.items()}


def print_stp(states):
    roots = root_names(states)
    for node, st in states.items():
        if st is None:
            print(f"{node}: spanning tree off or not running")
            continue
        root = " ROOT" if st["root"] == st["bridge"] else ""
        print(f"{node}: {'/'.join(st['protocol'])} bridge {fmt_id(st['bridge'])}"
              f" root {roots[node]} root-port {st['root-port'] or '-'}{root}")
        for port, p in sorted(st["ports"].items()):
            print(f"    {port:8} {p['role'] or '-':12} {p['state'] or '-'}")


def evaluate(states, want_root, want_blocked):
    """Problems with the converged tree, empty when there are none."""
    problems = []
    off = [n for n, st in states.items() if st is None]
    if off:
        problems.append(f"no spanning tree state on {', '.join(off)}"
                        " (Linux < 7.1 has no bridge stp_mode?)")
        return problems
    roots = {st["root"] for st in states.values()}
    if len(roots) != 1:
        problems.append(f"nodes disagree on the root: {sorted(map(fmt_id, roots))}")
        return problems
    root = roots.pop()
    lowest = min(st["bridge"] for st in states.values())
    if root != lowest:
        problems.append(f"root {fmt_id(root)} is not the lowest bridge {fmt_id(lowest)}")
    root_nodes = [n for n, st in states.items() if st["bridge"] == root]
    if want_root and root_nodes != [want_root]:
        problems.append(f"root is {root_nodes}, expected {want_root}")
    blocked = [f"{n}:{p}" for n, st in states.items()
               for p, ps in st["ports"].items()
               if ps["state"] not in ("FORWARDING", "DISABLED")]
    if len(blocked) != want_blocked:
        problems.append(f"{len(blocked)} ports not forwarding {blocked},"
                        f" expected {want_blocked}")
    return problems


def cmd_check(args):
    names = nodes()
    if not names:
        sys.exit("no switch is running")
    blocked = args.blocked
    if blocked is None:
        # Independent loops of the topology: links - nodes + 1 (connected).
        blocked = len(links()) - len(names) + 1
    deadline = time.monotonic() + args.timeout
    while True:
        states = stp_states(names)
        problems = evaluate(states, args.root, blocked)
        if not problems or time.monotonic() > deadline:
            break
        time.sleep(1)
    print_stp(states)
    if problems:
        for p in problems:
            print(f"FAIL: {p}")
        sys.exit(1)
    print(f"OK: one root, {blocked} blocked port(s)")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("nodes", help="list running switches")
    p = sub.add_parser("get", help="RESTCONF GET")
    p.add_argument("node")
    p.add_argument("path")
    for method in ("set", "put"):
        p = sub.add_parser(method, help=f"RESTCONF {'PATCH' if method == 'set' else 'PUT'}")
        p.add_argument("node")
        p.add_argument("path")
        p.add_argument("body", help="JSON")
    p = sub.add_parser("delete", help="RESTCONF DELETE")
    p.add_argument("node")
    p.add_argument("path")
    p = sub.add_parser("exec", help="run a command in a switch")
    p.add_argument("node")
    p.add_argument("command", nargs=argparse.REMAINDER)
    sub.add_parser("stp", help="spanning tree state of every switch")
    p = sub.add_parser("check", help="wait for a converged tree and check it")
    p.add_argument("--root", help="switch expected to be root")
    p.add_argument("--blocked", type=int,
                   help="ports expected not forwarding (default: loops in the topology)")
    p.add_argument("--timeout", type=float, default=30, help="seconds to wait")
    args = ap.parse_args()

    if args.cmd == "nodes":
        print("\n".join(nodes()))
    elif args.cmd in ("get", "set", "put", "delete"):
        method = {"get": "GET", "set": "PATCH", "put": "PUT", "delete": "DELETE"}[args.cmd]
        status, data = restconf(args.node, method, args.path, getattr(args, "body", None))
        if data is not None:
            print(json.dumps(data, indent=2))
        if status >= 300:
            sys.exit(f"HTTP {status}")
    elif args.cmd == "exec":
        sys.exit(compose("exec", "-T", args.node, *args.command).returncode)
    elif args.cmd == "stp":
        print_stp(stp_states(nodes()))
    elif args.cmd == "check":
        cmd_check(args)


if __name__ == "__main__":
    main()
