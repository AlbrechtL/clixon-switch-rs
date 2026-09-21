# Tests

```sh
cargo test                                          # the pure crates, on the host
dev/container.sh python3 -m pytest tests/switch     # one switch, in a container
```

`tests/switch` drives a single `clixon_backend` over RESTCONF and checks the
result in the kernel. `dev/in-container.sh` starts clixon on the factory
default before pytest runs, so the suite needs no setup of its own.

`tests/lib` holds the helpers both suites share. A `Node` bundles one
switch's RESTCONF API with views onto its kernel (`ip`, `bridge`, sysfs),
mstpd, its processes, its files and its SNMP agent. Those views run their
commands through a `Shell`, which is either local (inside the container under
test) or `docker exec` (a containerlab node), so the same assertions serve
both.

Each test starts from the factory default: an autouse fixture restores the
configuration captured at session start, in **setup** rather than teardown,
so a failing test leaves its state in the container to be inspected. Tests
marked `restart` also get the startup database back and a cold backend.

## Spanning tree needs Linux 7.1

A bridge passes BPDUs up to mstpd only once the kernel has left spanning tree
to userspace, and the kernel asks `/sbin/bridge-stp` about that only for
bridges in the host's network namespace. Linux 7.1 added the bridge's
`stp_mode`, which says so outright and works in any namespace.

On an older kernel, enabling spanning tree fails and every test needing it
**skips**, naming the running release. Nothing passes silently.

## Where each section of the old shell suite went

`tests/integration/run.sh` was the previous suite. Its sections map onto the
modules as follows.

| Section | Module |
| --- | --- |
| factory default | `test_factory_default.py` |
| web UI and system state, state data | `test_web_and_system.py` |
| spanning tree | `test_rstp.py` |
| MSTP | `test_mstp.py` |
| spanning tree off | `test_stp_lifecycle.py` |
| DHCP client | `test_dhcp_client.py` |
| SNMP | `test_snmp.py` |
| invalid configuration is rejected | `test_validation.py` |
| access VLAN, management address, unconfigured port, trunks, suspended VLAN, routed VLAN by name | `test_vlans.py` |
| port-based VLAN groups | `test_port_based_vlans.py` |
| persistent storage, restart, save, failsafe | `test_persistence.py` |
