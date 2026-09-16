# clixon-switch-rs

> **⚠️ Proof of concept.** This project is a proof of concept, created with
> the help of AI. It has not undergone thorough review or hardening, and
> should not be assumed suitable for production use.

A [clixon](https://www.clicon.org/) backend plugin, written in Rust, that
applies an OpenConfig switch configuration to the Linux kernel: DSA switch
ports in one VLAN-aware bridge, as 802.1Q access and trunk ports or in
port-based VLAN groups, and routed VLAN interfaces with static IPv4
addresses or a DHCP client.

clixon provides the datastores, the CLI, NETCONF and RESTCONF. This plugin
validates each commit and reconciles the kernel over netlink. It was written
for the Zyxel GS1900-8 running
[meta-ethernet-switch-os](https://github.com/AlbrechtL/meta-ethernet-switch-os),
but nothing in it is specific to that board.

## Data model

| Module | Used for |
|---|---|
| `openconfig-interfaces` | `interface[name]/config/{type,enabled}` |
| `openconfig-vlan` | switch ports: `ethernet/switched-vlan/config/{interface-mode,access-vlan,native-vlan,trunk-vlans}`; routed VLANs: `routed-vlan/config/vlan` (id or name) |
| `openconfig-if-ip` | `routed-vlan/ipv4/addresses/address[ip]/config/prefix-length`; `routed-vlan/ipv4/config/dhcp-client` |
| `clixon-switch` | `vlans/vlan[vlan-id]/config/{name,status}` (the `vlan-top` grouping of `openconfig-vlan`); `switch/config/vlan-mode`; `port-based-vlans/group[id]/config/{name,port}`; state only: `routed-vlan/ipv4/state/dhcp-lease` |

The switch runs in one of two VLAN modes, `switch/config/vlan-mode`:

- **`DOT1Q`** (default), tag-based. Every VLAN a port or routed VLAN
  interface uses must be declared in `vlans`. `ACCESS` ports carry their
  `access-vlan` untagged. `TRUNK` ports carry `native-vlan` untagged and the
  `trunk-vlans` tagged: ids, or ranges `"x..y"` that stand for the declared
  VLANs in them. A trunk without `trunk-vlans` carries every declared VLAN.
  A VLAN with `status SUSPENDED` stays declared but no port carries it.
- **`PORT_BASED`**, no tags. Each port is a member of exactly one
  `port-based-vlans` group and forwards only to ports of its group. The group
  id is the VLAN id the group uses inside the switch, so a routed VLAN
  interface refers to a group by its id or name. `vlans` and `switched-vlan`
  must be absent.

Configuration of the other mode is rejected, so switching modes takes one
commit that replaces both, e.g. a RESTCONF `PUT` on `/restconf/data` or
several edits in the CLI's candidate before `commit`.

The factory default declares VLAN 1, puts `lan1`..`lan8` into it as access
ports and gives the routed VLAN interface `vlan1` the address 192.168.1.1/24:

```json
{"clixon-switch:vlans": {"vlan": [
  {"vlan-id": 1, "config": {"vlan-id": 1, "name": "default"}}]},
 "openconfig-interfaces:interfaces": {"interface": [
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
does not implement (MTU, subinterfaces, IPv6, ...) when validating a commit,
instead of storing it silently. Leaves it does not implement are accepted only
with their YANG default, which clixon fills into every tree
(`crates/switch-model/src/supported.rs`). `deviate not-supported` would be
the YANG way, but clixon 7.8 still accepts data for such nodes and only skips
their `must` checks. The OpenConfig modules in
`yang/vendor` are the import closure copied from openconfig/public by
`scripts/vendor-yang.sh`. `openconfig-if-ip` has to stay at 3.7.0 or older,
because later versions import `openconfig-network-instance` and with it
BGP, IS-IS, MPLS and more. That is also why the VLAN database lives in
`clixon-switch`: in OpenConfig, only `openconfig-network-instance` uses the
`vlan-top` grouping.

`clixon.xml` sets `CLICON_VALIDATE_TARGET_STATE` to false. Otherwise clixon
7.8 merges the plugin's state data into the tree it validates, and removing
the last entry of a list (e.g. the only declared VLAN) fails, because the
entry's state recreates it without its `config`.

### Kernel mapping

| Configuration | Kernel |
|---|---|
| (always) | bridge `br-lan`, `vlan_filtering 1`, `vlan_default_pvid 0`; `lo` and the DSA conduit up |
| port, `access-vlan N` | port in `br-lan`, `bridge vlan add vid N pvid untagged`, up/down from `enabled` |
| port, `TRUNK`, `native-vlan N`, `trunk-vlans` | as above, plus `bridge vlan add vid T` for every trunk VLAN T other than N |
| port in port-based group N | as `access-vlan N` |
| VLAN N `SUSPENDED` | no `bridge vlan` entries for N on ports or on `br-lan` |
| switch port not configured | taken out of `br-lan`, down |
| `l3ipvlan` interface on VLAN or group N | 802.1Q link on `br-lan` with id N, `bridge vlan add dev br-lan vid N self`, its addresses |
| `ipv4/config/dhcp-client true` | `udhcpc` on that interface, a child of `clixon_backend` |

A port's new PVID entry is added before its other entries change, so the port
never drops untagged frames while its native VLAN moves. The rtl83xx DSA
driver offloads all of these entries to the switch chip, including groups,
which are ordinary VLANs.

### DHCP client

At most one routed VLAN interface may set `ipv4/config/dhcp-client`,
because the client owns the default route and `resolv.conf`. Static
addresses on the same interface stay. The factory default does not use DHCP.
To enable it on `vlan1`:

```sh
curl -X PATCH -H 'Content-Type: application/yang-data+json' \
  -d '{"openconfig-if-ip:ipv4":{"config":{"dhcp-client":true}}}' \
  http://192.168.1.1/restconf/data/openconfig-interfaces:interfaces/interface=vlan1/openconfig-vlan:routed-vlan/openconfig-if-ip:ipv4
```

The plugin runs busybox `udhcpc` with `scripts/udhcpc-script.sh`, which it
expects in the directory above `CLICON_BACKEND_DIR`. On each lease the script
- adds the address with the lease time as its lifetime (`valid_lft`), so
  the kernel marks it dynamic. That is how the planner tells it from static
  addresses, which it would otherwise remove on the next commit. If the
  client dies, the address expires.
- replaces the default route with the first router.
- writes the DNS servers and domain to `resolv.conf` (`RESOLV_CONF` in the
  backend's environment, default `/etc/resolv.conf`; a symlink is followed).
- records the lease in `lease.<interface>` in `CLICON_XMLDB_DIR`.

udhcpc sends the host name and releases the lease when the client is
stopped. A backend that starts stops any udhcpc a previous one left behind
(pid files in `CLICON_XMLDB_DIR`).

State data shows each address with its `origin` (`STATIC` or `DHCP`),
`ipv4/state/dhcp-client`, and, since OpenConfig does not model the lease,
the `clixon-switch:dhcp-lease` container: address, routers, DNS servers,
domain, server, lease time and remaining time.

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

A `startup_db` saved before the VLAN database existed has no `vlans`, so
it fails validation and the switch comes up with the failsafe factory
default.

## Layout

| Path | Content |
|---|---|
| `crates/switch-model` | RFC 7951 JSON → validated `DesiredState`; state data XML. Pure, host-tested. |
| `crates/switch-net` | `ActualState`, the planner, `reconcile`, the netlink backend and a kernel-like fake for tests |
| `crates/clixon-sys` | hand-written declarations for the libclixon 7.8 subset in use |
| `crates/clixon-plugin` | safe plugin interface: callbacks, panics caught, logging, transactions |
| `crates/clixon-switch-plugin` | the cdylib clixon loads |
| `clixon/` | `clixon.xml` template, `autocli.xml`, CLI spec |
| `scripts/` | factory default generator, `prepare-datastore`, udhcpc script, YANG vendoring |
| `dev/` | development container with clixon at the Yocto recipes' revisions |
| `tests/integration/` | RESTCONF tests against clixon in the container |

## Development

1. **Unit tests, on the host, seconds.** They need no clixon: model,
   validation and planner run against the fake kernel.

   ```sh
   cargo test
   ```

2. **Integration tests, in a container.** The container runs clixon with the
   plugin on dummy links `lan1`..`lan7` and a veth `lan8` with a busybox
   DHCP server at its other end, in its own network namespace. The tests
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
