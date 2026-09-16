# clixon-switch-rs

> **⚠️ Proof of concept.** This project is a proof of concept, created with
> the help of AI. It has not undergone thorough review or hardening, and
> should not be assumed suitable for production use.

A [clixon](https://www.clicon.org/) backend plugin, written in Rust, that
applies an OpenConfig switch configuration to the Linux kernel: DSA switch
ports in one VLAN-aware bridge, and routed VLAN interfaces with IPv4
addresses.

clixon provides the datastores, the CLI, NETCONF and RESTCONF. This plugin
validates each commit and reconciles the kernel over netlink. It was written
for the Zyxel GS1900-8 running
[meta-ethernet-switch-os](https://github.com/AlbrechtL/meta-ethernet-switch-os),
but nothing in it is specific to that board.

## Data model

| Module | Used for |
|---|---|
| `openconfig-interfaces` | `interface[name]/config/{type,enabled}` |
| `openconfig-vlan` | switch ports: `ethernet/switched-vlan/config/{interface-mode,access-vlan}`; routed VLANs: `routed-vlan/config/vlan` |
| `openconfig-if-ip` | `routed-vlan/ipv4/addresses/address[ip]/config/prefix-length` |

The factory default puts `lan1`..`lan8` into VLAN 1 as access ports and
gives the routed VLAN interface `vlan1` the address 192.168.1.1/24:

```json
{"openconfig-interfaces:interfaces": {"interface": [
  {"name": "lan1",
   "config": {"name": "lan1", "type": "iana-if-type:ethernetCsmacd", "enabled": true},
   "openconfig-if-ethernet:ethernet": {"openconfig-vlan:switched-vlan":
     {"config": {"interface-mode": "ACCESS", "access-vlan": 1}}}},
  {"name": "vlan1",
   "config": {"name": "vlan1", "type": "iana-if-type:l3ipvlan", "enabled": true},
   "openconfig-vlan:routed-vlan": {"config": {"vlan": 1},
     "openconfig-if-ip:ipv4": {"addresses": {"address": [
       {"ip": "192.168.1.1", "config": {"ip": "192.168.1.1", "prefix-length": 24}}]}}}}
]}}
```

`yang/clixon-switch@*.yang` is the main module. The plugin rejects what it
does not implement (MTU, trunk ports, IPv6, ...) when validating a commit,
instead of storing it silently. Leaves it does not implement are accepted only
with their YANG default, which clixon fills into every tree
(`crates/switch-model/src/supported.rs`). `deviate not-supported` would be
the YANG way, but clixon 7.8 still accepts data for such nodes and only skips
their `must` checks. The OpenConfig modules in
`yang/vendor` are the import closure copied from openconfig/public by
`scripts/vendor-yang.sh`. `openconfig-if-ip` has to stay at 3.7.0 or older,
because later versions import `openconfig-network-instance` and with it
BGP, IS-IS, MPLS and more.

### Kernel mapping

| Configuration | Kernel |
|---|---|
| (always) | bridge `br-lan`, `vlan_filtering 1`, `vlan_default_pvid 0`; `lo` and the DSA conduit up |
| port, `access-vlan N` | port in `br-lan`, `bridge vlan add vid N pvid untagged`, up/down from `enabled` |
| switch port not configured | taken out of `br-lan`, down |
| `l3ipvlan` interface on VLAN N | 802.1Q link on `br-lan` with id N, `bridge vlan add dev br-lan vid N self`, its addresses |

Switch ports are the DSA user ports. `CLIXON_SWITCH_PORTS="lan1 lan2"` in the
environment of `clixon_backend` names other links instead, e.g. dummy links
in a test container.

### Persistence

Commits change the running configuration only. `save` in the CLI, or a
NETCONF/RESTCONF `copy-config` from running to startup, makes it persistent.

Before `clixon_backend` starts, `prepare-datastore` sets up the datastore
directory:
- It creates `startup_db` from the factory default if there is none yet.
  Deleting `startup_db` is a factory reset.
- It always installs the factory default as `failsafe_db`. clixon falls back
  to it when `startup_db` fails to commit.

## Layout

| Path | Content |
|---|---|
| `crates/switch-model` | RFC 7951 JSON → validated `DesiredState`; state data XML. Pure, host-tested. |
| `crates/switch-net` | `ActualState`, the planner, `reconcile`, the netlink backend and a kernel-like fake for tests |
| `crates/clixon-sys` | hand-written declarations for the libclixon 7.8 subset in use |
| `crates/clixon-plugin` | safe plugin interface: callbacks, panics caught, logging, transactions |
| `crates/clixon-switch-plugin` | the cdylib clixon loads |
| `clixon/` | `clixon.xml` template, `autocli.xml`, CLI spec |
| `scripts/` | factory default generator, `prepare-datastore`, YANG vendoring |
| `dev/` | development container with clixon at the Yocto recipes' revisions |
| `tests/integration/` | RESTCONF tests against clixon in the container |

## Development

1. **Unit tests, on the host, seconds.** They need no clixon: model,
   validation and planner run against the fake kernel.

   ```sh
   cargo test
   ```

2. **Integration tests, in a container.** The container runs clixon with the
   plugin on dummy links `lan1`..`lan8` in its own network namespace. The tests
   drive RESTCONF and check the kernel with `ip` and `bridge`.

   ```sh
   dev/container.sh tests/integration/run.sh
   dev/container.sh            # shell: clixon_cli -f /usr/local/etc/clixon.xml, curl localhost:8080
   ```

3. **On the switch.** Build with Yocto from a local checkout, then run the
   plugin from RAM without flashing:

   ```sh
   devtool modify -n clixon-switch ~/src/clixon-switch-rs    # once
   devtool build clixon-switch
   scripts/deploy.sh root@192.168.1.1
   ```

   After changing `Cargo.lock`, regenerate the recipe's crate list with
   `bitbake -c update_crates clixon-switch`.

## License

Apache-2.0. The vendored OpenConfig modules are Apache-2.0 as well; the IETF
and IANA modules are BSD-2-Clause (see their headers).
