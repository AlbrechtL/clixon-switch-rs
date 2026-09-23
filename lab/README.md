# Multi-switch lab

Several clixon-switch-rs instances, one per container, wired together by
docker compose. `gen-compose.py` writes the compose files:

- `compose.yaml`: five switches in a ring running RSTP, with `sw1` as root.

  ```
  sw1 lan2 -- lan1 sw2 lan2 -- lan1 sw3 lan2 -- lan1 sw4 lan2 -- lan1 sw5
   lan1 ------------------------------------------------------------ lan2
  ```

- `chain.yaml`: 38 bridges in a line, `r1 - s1 - ... - s36 - r2`, `r1` with
  priority 0 and `r2` with 4096. That is longer than the default max age of
  20 allows. For `test-max-age.py`.

```sh
lab/gen-compose.py ring > lab/compose.yaml
lab/gen-compose.py chain > lab/chain.yaml
lab/gen-compose.py chain 50 --max-age 40 --forwarding-delay 21   # variants
```

Spanning tree needs **Linux 7.1** on the host (the bridge's `stp_mode`, see
the top-level README). On an older kernel the backends stop at their first
commit, and `up.sh` warns about it.

```sh
lab/up.sh                                  # build images, start, wait until healthy
lab/labctl.py check --root sw1             # converged: one root, one blocked port
lab/labctl.py stp                          # role and state of every port
docker compose -f lab/compose.yaml down    # tear down
```

## How it works

- **Image.** `up.sh` builds the dev image (`dev/Dockerfile`) and then
  `lab/Dockerfile` on top of it, which builds and installs the plugin once.
  The containers start without compiling.
- **Links.** Every link is a docker network with two endpoints. Its host
  bridge runs no spanning tree, so it passes BPDUs like a cable. Compose's
  `interface_name` names the ports `lan1`, `lan2`, ... in each container.
  The link networks are IPv6 only, because docker has only about 30 IPv4
  subnets to give out. The entry point removes docker's addresses and routes
  from the ports. The link networks must not be `internal`: docker then drops
  bridged IPv4 whose addresses are not in the network's subnet, i.e. all
  traffic between the switches. `eth0` on the default network is for
  management only.
- **Initial configuration.** `entrypoint.sh` turns the service's environment
  into the startup datastore (`startup-config.sh`), then starts
  `clixon_backend` and `clixon_restconf`. Spanning tree has to be in the
  startup configuration, not set afterwards: in a ring, ports that join
  `br-lan` before spanning tree runs make a broadcast storm.
- **Control.** `labctl.py` reaches every switch through `docker compose
  exec`: RESTCONF with curl on `localhost:8080`, or any other command. The
  host needs nothing but docker and Python 3.

## Environment of a switch

| Variable | Default | Meaning |
|---|---|---|
| `SWITCH_PORTS` | `lan1 lan2` | switch ports, access ports in VLAN 1 |
| `SWITCH_ADDRESS` | `192.168.1.1/24` | address of `vlan1`; give every switch its own (the generated files use 192.168.0.0/16) |
| `STP_PROTOCOL` | (off) | `RSTP`, `MSTP` or `STP` |
| `STP_BRIDGE_PRIORITY` | `32768` | bridge priority, in steps of 4096 |
| `STP_MAX_AGE` | (20) | max age |
| `STP_FORWARDING_DELAY` | (15) | forwarding delay; `2 * (forwarding delay - 1) >= max age` |
| `STP_EDGE_PORTS` | | ports with `edge-port EDGE_ENABLE` |
| `SWITCH_CONFIG_FILE` | | a datastore file (mounted into the container), used instead of all of the above |

## labctl.py

```sh
lab/labctl.py nodes
lab/labctl.py get sw1 openconfig-spanning-tree:stp
lab/labctl.py set sw3 openconfig-spanning-tree:stp/rstp/config \
    '{"openconfig-spanning-tree:config":{"bridge-priority":0}}'   # PATCH
lab/labctl.py put sw3 <path> <json>
lab/labctl.py delete sw3 <path>
lab/labctl.py exec sw2 mstpctl showport br-lan
lab/labctl.py stp
lab/labctl.py check [--root sw1] [--blocked N] [--timeout 30]
```

`check` waits until the tree has converged or the timeout has passed: every
switch agrees on the root, the root is the lowest bridge ID (and `--root`,
if given), and exactly `--blocked` ports do not forward. The default for
`--blocked` is the number of loops in the topology, links - switches + 1.
Ports that are down do not count. `stp` and `check` read `stp/rstp`, so
they cover STP and RSTP, not MSTP yet.

`LAB_COMPOSE` selects another compose file, e.g. for another topology.

## Proof of concept

```sh
lab/up.sh
lab/labctl.py check --root sw1
lab/labctl.py set sw3 openconfig-spanning-tree:stp/rstp/config \
    '{"openconfig-spanning-tree:config":{"bridge-priority":0}}'
lab/labctl.py check --root sw3
lab/labctl.py exec sw2 ip link set lan2 down       # break the ring
lab/labctl.py check --root sw3 --blocked 0
lab/labctl.py exec sw1 busybox ping -c 3 192.168.1.4
```

## Max age

`test-max-age.py` follows Vincent Bernat's
[spanning tree article](https://vincent.bernat.ch/en/blog/2026-spanning-tree).
Each bridge adds one to a BPDU's message age and discards BPDUs whose
message age has reached the max age. So the max age limits how far a bridge
can be from the root. All bridges use the timers in the root's BPDUs, so
only the root's max age counts.

```sh
lab/up.sh -f lab/chain.yaml        # 38 containers, about a minute
lab/test-max-age.py
docker compose -f lab/chain.yaml down
```

1. Max age 20, the default: `r1`, `s1`..`s20` have root `r1`; `s21`..`s36`
   and `r2` have root `r2`. Between `s20` and `s21`, `s21` discards `s20`'s
   BPDUs (message age 20) and `s20` discards `s21`'s (`r2` is worse than `r1`).
2. Max age 40 and forwarding delay 21 on `r1` only: every bridge has root `r1`.
3. Max age 20 and forwarding delay 15 on `r1` again: the chain splits again.

The chain has no loop, so every port forwards in every phase. The script
waits up to `--timeout` seconds (default 90) for each phase and stops at
the first phase that fails.
