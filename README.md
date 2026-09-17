# clixon-switch-rs

> **⚠️ Proof of concept.** This project is a proof of concept, created with
> the help of AI. It has not undergone thorough review or hardening, and
> should not be assumed suitable for production use.

A [clixon](https://www.clicon.org/) backend plugin, written in Rust, that
applies an OpenConfig switch configuration to the Linux kernel: DSA switch
ports in one VLAN-aware bridge, as 802.1Q access and trunk ports or in
port-based VLAN groups, routed VLAN interfaces with static IPv4
addresses or a DHCP client, spanning tree (STP, RSTP, MSTP) with mstpd, and
a read-only SNMPv3 agent (net-snmp's snmpd with clixon_snmp) for the system
group, IF-MIB, BRIDGE-MIB, Q-BRIDGE-MIB and RSTP-MIB.

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
| `openconfig-spanning-tree` | `stp/global/config/{enabled-protocol,bpdu-guard,bpdu-filter}`; `stp/rstp/config/{hello-time,max-age,forwarding-delay,hold-count,bridge-priority}` and `stp/rstp/interfaces`; `stp/mstp/config/{name,revision,max-hop,...timers}`, `stp/mstp/mst-instances/mst-instance[mst-id]/config/{vlan,bridge-priority}` and its `interfaces`; `stp/interfaces/interface[name]/config/{edge-port,link-type,guard,bpdu-guard,bpdu-filter}` |
| `ietf-snmp` | `snmp/engine/{enabled,listen,version/v3,engine-id}`; `snmp/usm/local/user[name]/{auth/sha,priv/aes}/key`; `snmp/vacm/group[name]/{member,access}`, `snmp/vacm/view[name]/{include,exclude}` |
| `clixon-switch` | `vlans/vlan[vlan-id]/config/{name,status}` (the `vlan-top` grouping of `openconfig-vlan`); `switch/config/vlan-mode`; `port-based-vlans/group[id]/config/{name,port}`; `system/config/{contact,location}`; identity `STP`; the MSTP CIST: `stp/mstp/config/bridge-priority`, `stp/mstp/interfaces`; state only: `routed-vlan/ipv4/state/dhcp-lease`, `stp/mstp/state` of the CIST, `snmp/engine/engine-id-in-use` |
| `BRIDGE-MIB`, `Q-BRIDGE-MIB`, `RSTP-MIB` | state only, for SNMP (see below) |

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
their `must` checks. The modules in `yang/vendor` are the import closure
copied from openconfig/public and, for `ietf-snmp` and `ietf-yang-smiv2`,
from YangModels/yang by `scripts/vendor-yang.sh`. `yang/mib` holds the MIBs
translated to YANG by `scripts/mib-to-yang.sh` (run it before
`vendor-yang.sh`). `openconfig-if-ip` has to stay at 3.7.0 or older,
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
| (always) | `br-lan` with `mst_enabled 1`: per-VLAN spanning tree states |
| `stp/global/config/enabled-protocol` set | `mstpd` a child of `clixon_backend`, `br-lan` `stp_state` 1 before ports join, mstpd configured with `mstpctl` |
| MSTI with `vlan` | `bridge vlan global set vid V msti M`, and the ports' MSTI states, programmed by the patched mstpd |
| `snmp/engine/enabled true` | `snmpd` and `clixon_snmp`, children of `clixon_backend`; `snmpd.conf` in `CLICON_XMLDB_DIR` |

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

### Spanning tree

Spanning tree is off in the factory default. `stp/global/config/enabled-protocol`
turns it on, with one of:

- `openconfig-spanning-tree-types:RSTP`, configured in `stp/rstp`.
- `openconfig-spanning-tree-types:MSTP`, configured in `stp/mstp`: the region
  (`name`, `revision`), `max-hop`, the timers and the MSTIs. OpenConfig has
  no place for the CIST's bridge priority and port settings, so
  `clixon-switch` adds `bridge-priority` to `stp/mstp/config` and an
  `interfaces` list to `stp/mstp`, built from OpenConfig's own groupings.
- `clixon-switch:STP`, IEEE 802.1D STP, configured in `stp/rstp` like RSTP.
  OpenConfig has no identity for it; `clixon-switch` derives one from
  `oc-stp-types:STP_PROTOCOL`.

To turn on RSTP, with a lower bridge priority and `lan8` as edge port
(`/stp` does not exist before, so the PATCH goes to the datastore):

```sh
curl -X PATCH -H 'Content-Type: application/yang-data+json' \
  -d '{"ietf-restconf:data":{"openconfig-spanning-tree:stp":{
        "global":{"config":{"enabled-protocol":["openconfig-spanning-tree-types:RSTP"]}},
        "rstp":{"config":{"bridge-priority":4096}},
        "interfaces":{"interface":[{"name":"lan8","config":{"name":"lan8",
          "edge-port":"openconfig-spanning-tree-types:EDGE_ENABLE"}}]}}}}' \
  http://192.168.1.1/restconf/data
```

RSTP and MSTP fall back to STP on a port with an 802.1D neighbour by
themselves. Only one protocol may be enabled. The container of the protocol not
in use may keep its configuration; it is validated but has no effect.

Every switch port takes part. `stp/*/interfaces` entries only change a
port's cost and priority, `stp/interfaces` its edge mode (default
`EDGE_AUTO`), link type (default detected), `guard ROOT`, BPDU guard and BPDU
filter; `stp/global/config` sets the latter two for all ports. The values
must be what 802.1D encodes: bridge priorities in steps of 4096, port
priorities in steps of 16, `hello-time` 2 (mstpd supports no other),
`2 * (forwarding-delay - 1) >= max-age`, `max-hop` 6..40. An MSTI's `vlan`
list (ids and ranges `x..y`) counts as configured, declared or not, because
the MST configuration digest must match the other bridges of the region; a
VLAN belongs to at most one MSTI. Not implemented, and rejected: rapid PVST,
loop guard, bridge assurance, EtherChannel guard, BPDU guard recovery.

A commit that enables spanning tree starts `mstpd` (in the foreground, logging
to syslog) before it touches the kernel, sets `stp_state` on `br-lan` so the
kernel runs `/sbin/bridge-stp`, which leaves spanning tree to userspace
(`scripts/bridge-stp.sh`), and configures mstpd with `mstpctl`. It only runs
the `mstpctl` commands whose values changed, and all of them after mstpd or
the bridge was restarted or ports joined. Turning spanning tree off removes
the bridge from mstpd, which maps all VLANs back to the CIST, and stops it.
A backend that starts stops any mstpd a previous one left behind.

Plain mstpd computes MSTI states but only applies the CIST's. The mstpd of
[meta-ethernet-switch-os](https://github.com/AlbrechtL/meta-ethernet-switch-os)
carries a patch that programs the kernel's
per-VLAN spanning tree: `br-lan` always has `mst_enabled`, which can only be
switched while no port has VLANs, and mstpd maps VLANs to MSTIs and sets the
ports' MSTI states, again whenever the kernel reports a VLAN change. The
rtl83xx DSA driver offloads both.

State data comes from `mstpctl -f json`: bridge and root identifiers, root
port and cost, topology changes; per port the role, state, designated bridge
and port, forward transitions and BPDU counters. mstpd reports RSTP states:
`discarding` shows as `BLOCKING`.

State data shows each address with its `origin` (`STATIC` or `DHCP`),
`ipv4/state/dhcp-client`, and, since OpenConfig does not model the lease,
the `clixon-switch:dhcp-lease` container: address, routers, DNS servers,
domain, server, lease time and remaining time.

Switch ports are the DSA user ports. `CLIXON_SWITCH_PORTS="lan1 lan2"` in the
environment of `clixon_backend` names other links instead, e.g. dummy links
in a test container.

### SNMP

SNMP is off in the factory default. When enabled, it is SNMPv3 only and
read-only: the configuration (the datastore) stays the only source of truth,
and SNMP only shows it and the state of the switch.

Two daemons serve it, both started by the plugin while
`snmp/engine/enabled` is true:
- **`snmpd`** (net-snmp) owns UDP port 161: SNMPv3 security (USM) and access
  control (VACM). It answers SNMPv2-MIB's system group and IF-MIB (ports,
  counters) itself, from the kernel.
- **`clixon_snmp`** is its AgentX subagent for BRIDGE-MIB, Q-BRIDGE-MIB and
  RSTP-MIB. It asks `clixon_backend` like any other clixon client. The plugin
  answers from the running configuration, the kernel (bridge VLANs,
  forwarding database) and mstpd.

```
manager --UDP 161, SNMPv3--> snmpd -- system group, IF-MIB
                               |
                               +--AgentX socket--> clixon_snmp --> clixon_backend + plugin:
                                                   1.3.6.1.2.1.17  BRIDGE-MIB, Q-BRIDGE-MIB, RSTP-MIB
```

The MIB modules are translations to YANG (RFC 6643, `yang/mib`), in which
every object is `config false`, so SNMP cannot change anything. snmpd's
configuration grants no write views either. `scripts/mib-to-yang.sh` makes
them with libsmi's smidump from the MIBs of libsmi 0.5.0, with two fixes to
smidump's output (in its comments): Q-BRIDGE-MIB would pull in RMON2-MIB and
some 16000 lines of YANG for one type, and RSTP-MIB repeats BRIDGE-MIB's
spanning tree scalars.

The configuration is `/snmp` of `ietf-snmp` (RFC 7407). The plugin
accepts:
- `engine`: `enabled`, `listen` entries with `udp` on IPv4 addresses,
  `version/v3` (required, the only one), `engine-id` (optional).
- `usm/local/user`: `auth/sha` (required) and `priv/aes` (optional), with
  localized keys (20 and 16 octets).
- `vacm/group`: `member`s (security model `usm`) and `access` with context
  `""`, security model `usm` or `any`, security level `auth-no-priv` or
  `auth-priv`, and a `read-view`.
- `vacm/view`: `include` and `exclude` with numeric OIDs.

It rejects SNMPv1/v2c and communities, notifications (`target`,
`target-params`, `notify-filter-profile`), proxies, TLS/SSH, remote users,
MD5 and DES, `no-auth-no-priv`, contexts, `write-view` and `notify-view`,
and OID wildcards. `system/config/{contact,location}` in `clixon-switch` are
sysContact and sysLocation; sysName is the host name.

USM keys in `ietf-snmp` are localized keys: derived from a passphrase and
the engine ID (RFC 3414 A.2). Without `engine-id`, the engine ID is derived
from `br-lan`'s MAC address (RFC 3411 format 3 with Net-SNMP's enterprise
number); `snmp/engine/clixon-switch:engine-id-in-use` shows it. So pick the
engine ID first, then make the keys on your own computer, so the passphrases
never go to the switch:

```sh
scripts/snmp-localize-key --engine-id 80:00:1f:88:04:73:77:31 \
    --auth 'auth passphrase' --priv 'priv passphrase'
curl -X PUT -H 'Content-Type: application/yang-data+json' -d '{"ietf-snmp:snmp": {
    "engine": {"enabled": true, "engine-id": "80:00:1f:88:04:73:77:31",
               "version": {"v3": [null]},
               "listen": [{"name": "all", "udp": {"ip": "0.0.0.0"}}]},
    "usm": {"local": {"user": [{"name": "nms",
        "auth": {"sha": {"key": "<auth sha key>"}}, "priv": {"aes": {"key": "<priv aes key>"}}}]}},
    "vacm": {"group": [{"name": "readers",
                        "member": [{"security-name": "nms", "security-model": ["usm"]}],
                        "access": [{"context": "", "security-model": "usm",
                                    "security-level": "auth-priv", "read-view": "all"}]}],
             "view": [{"name": "all", "include": ["1.3.6.1"]}]}}}' \
  http://192.168.1.1/restconf/data/ietf-snmp:snmp
snmpwalk -v3 -l authPriv -u nms -a SHA -A 'auth passphrase' -x AES -X 'priv passphrase' \
  192.168.1.1 1.3.6.1.2.1.17
```

What the bridge MIBs show:
- **BRIDGE-MIB**: the bridge address and ports (`dot1dBasePort` numbers
  ports by the number in their names, `lan1` is 1; `dot1dBasePortIfIndex` is
  IF-MIB's ifIndex), the forwarding database, and while spanning tree runs,
  `dot1dStp` and the port table of the CIST.
- **RSTP-MIB**: the protocol version and hold count, and per port edge and
  point-to-point, while spanning tree runs. MSTP shows as `rstp`; the MSTIs
  have no MIB here (IEEE8021-MSTP-MIB is not in libsmi).
- **Q-BRIDGE-MIB**: the VLANs as configured (`dot1qVlanStaticTable`, in
  port-based mode the groups) and as in the kernel (`dot1qVlanCurrentTable`),
  PVIDs and acceptable frame types, and the forwarding database per VLAN.

A commit that changes what `snmpd.conf` would say restarts both daemons.
snmpd keeps `engineBoots` in its persistent directory (`/var/lib/net-snmp`,
`CLIXON_SWITCH_SNMP_PERSISTENT_DIR`), where it also saves its users. The
plugin removes those before every start, so a user taken out of the
configuration cannot come back. `snmpd.conf` holds the keys and is readable
by root only. A backend that starts stops any snmpd and clixon_snmp a
previous one left behind.

clixon 7.8's `clixon_snmp` needed fixes for these MIBs: binary and
mac-address types, tables with augments, index leaves of other tables, and
SMI defaults in state data. meta-ethernet-switch-os carries them as patches
(`recipes-clixon/clixon/files`), and the dev container applies them.

### System state and status page

`/system/state` (`clixon-switch`) is state data: the contact and location
of `/system/config`, host name, the firmware's `NAME` and `VERSION` from
`/etc/os-release`, kernel release, clock, uptime, load averages and memory.

`clixon_restconf` serves `www/` at `/` (clixon's `http-data`, see
`clixon.xml`): a read-only status page with the system state, the routed
VLAN interfaces with their addresses and DHCP lease, the ports and the VLANs
or port-based groups. It uses nothing but RESTCONF GETs on the same origin,
refreshes every 5 seconds, and links to SWUpdate's web UI on port 8080.
There is no authentication yet, like RESTCONF itself.

```sh
curl -H 'Accept: application/yang-data+json' \
  http://192.168.1.1/restconf/data/clixon-switch:system
```

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
| `crates/switch-model` | RFC 7951 JSON → validated `DesiredState`; state data XML, also of the bridge MIBs; `snmpd.conf`. Pure, host-tested. |
| `crates/switch-net` | `ActualState`, the planner, `reconcile`, the netlink backend (also the bridge FDB) and a kernel-like fake for tests; the child processes: DHCP client, mstpd, snmpd and clixon_snmp |
| `crates/clixon-sys` | hand-written declarations for the libclixon 7.8 subset in use |
| `crates/clixon-plugin` | safe plugin interface: callbacks, panics caught, logging, transactions |
| `crates/clixon-switch-plugin` | the cdylib clixon loads |
| `clixon/` | `clixon.xml` template, `autocli.xml`, CLI spec |
| `www/` | the status page (plain HTML, CSS, JavaScript), served by `clixon_restconf` |
| `scripts/` | factory default generator, `prepare-datastore`, udhcpc script, `/sbin/bridge-stp`, YANG vendoring, MIB translation, `snmp-localize-key` |
| `yang/` | the main module; `vendor/` imported modules; `mib/` MIBs translated to YANG |
| `dev/` | development container with clixon and mstpd at the Yocto recipes' revisions and with the layer's patches, net-snmp, smidump |
| `tests/integration/` | RESTCONF tests against clixon in the container |

## Development

1. **Unit tests, on the host, seconds.** They need no clixon: model,
   validation and planner run against the fake kernel.

   ```sh
   cargo test
   ```

2. **Integration tests, in a container.** The container runs clixon with the
   plugin on dummy links `lan1`..`lan7` and a veth `lan8` with a busybox
   DHCP server at its other end, in its own network namespace, mstpd, and
   snmpd. The tests drive RESTCONF and check the kernel with `ip` and
   `bridge`, and SNMP with net-snmp's `snmpget` and `snmpwalk`.

   Spanning tree cannot converge there: the kernel only hands spanning tree
   to userspace for bridges in the host's network namespace, and a bridge
   without it forwards BPDUs instead of passing them to mstpd.
   `CLIXON_SWITCH_STP_IN_NETNS` makes the plugin leave `stp_state` alone, so
   the tests still cover mstpd's configuration, state data and the kernel's
   per-VLAN states. Loops have to be tested on the switch. The container
   needs `CAP_SYS_ADMIN`, because mstpd answers mstpctl with the client's
   credentials attached.

   ```sh
   dev/container.sh tests/integration/run.sh
   dev/container.sh            # shell: clixon_cli -f /usr/local/etc/clixon.xml, curl localhost:8080
   ```

   The image takes the clixon and mstpd patches from meta-ethernet-switch-os
   on GitHub (`LAYER_REV`, default master), or from a local checkout:
   `LAYER=~/src/.../meta-ethernet-switch-os dev/container.sh ...`.

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
and IANA modules are BSD-2-Clause (see their headers). The MIB translations in
`yang/mib` are derived from the IETF MIBs' RFCs (see their descriptions).
