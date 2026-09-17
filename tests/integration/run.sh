#!/bin/sh
# Integration tests against clixon_backend with the plugin, driven over
# RESTCONF and checked in the kernel. Runs inside the dev container, where
# dev/in-container.sh has started clixon on the factory default:
#
#   dev/container.sh tests/integration/run.sh
#
# The tests build on each other and run in order.

set -eu

CONFIG=/usr/local/etc/clixon.xml
XMLDB=/usr/local/var/run/clixon-switch
PERSISTENT=/usr/local/var/lib/clixon/clixon-switch
RC=http://localhost:8080/restconf
IFACES="$RC/data/openconfig-interfaces:interfaces"
VLAN1_IPV4="$IFACES/interface=vlan1/openconfig-vlan:routed-vlan/openconfig-if-ip:ipv4/addresses"
VLANS="$RC/data/clixon-switch:vlans"

failures=0

check() {
    # check <description> <expected> <actual>
    if [ "$2" = "$3" ]; then
        echo "ok   $1"
    else
        echo "FAIL $1: expected '$2', got '$3'"
        failures=$((failures + 1))
    fi
}

# request <method> <url> [json body]: prints the HTTP status, body in /tmp/body
request() {
    curl -sS -o /tmp/body -w '%{http_code}' -X "$1" \
        -H 'Content-Type: application/yang-data+json' \
        -H 'Accept: application/yang-data+json' \
        ${3:+-d "$3"} "$2"
}

# declare_vlans <id>...: adds the VLANs to the VLAN database
declare_vlans() {
    entries=$(for id in "$@"; do printf '{"vlan-id":%s,"config":{"vlan-id":%s}}\n' "$id" "$id"; done | jq -sc .)
    status=$(request PATCH "$VLANS" "{\"clixon-switch:vlans\":{\"vlan\":$entries}}")
    check "declare VLANs $*" 204 "$status"
}

rejected() {
    # rejected <description> <method> <url> <body>
    status=$(request "$2" "$3" "$4")
    if [ "$status" -ge 400 ]; then
        echo "ok   $1 (HTTP $status)"
    else
        echo "FAIL $1: accepted with HTTP $status"
        failures=$((failures + 1))
    fi
}

master() { ip -j link show "$1" | jq -r '.[0].master // "none"'; }
operstate_up() { ip -j link show "$1" | jq -r '.[0].flags | index("UP") != null'; }
vlans() { bridge -j vlan show dev "$1" | jq -r '[.[0].vlans[]? | "\(.vlan):\(.flags // [] | join(","))"] | join(" ")'; }
self_vlans() { bridge -j vlan show dev br-lan | jq -r '[.[] | select(.ifname == "br-lan") | .vlans[]? | .vlan] | join(" ")'; }
switched_vlan() { echo "$IFACES/interface=$1/openconfig-if-ethernet:ethernet/openconfig-vlan:switched-vlan/config"; }
link_exists() { ip link show "$1" >/dev/null 2>&1 && echo true || echo false; }
addresses() { ip -j addr show dev "$1" 2>/dev/null | jq -r '[.[0].addr_info[]? | select(.family == "inet") | "\(.local)/\(.prefixlen)"] | join(" ")'; }
dynamic_addresses() { ip -j addr show dev "$1" 2>/dev/null | jq -r '[.[0].addr_info[]? | select(.family == "inet" and .dynamic) | "\(.local)/\(.prefixlen)"] | join(" ")'; }
default_route() { ip -j route show default | jq -r '[.[] | "\(.gateway) \(.dev)"] | join(" ")'; }
udhcpc_count() { pgrep -xc udhcpc || true; }

# wait_until <seconds> <command...>: until the command succeeds
wait_until() {
    limit=$(($1 * 10))
    shift
    i=0
    until "$@"; do
        i=$((i + 1))
        [ $i -le $limit ] || return 1
        sleep 0.1
    done
}
has_dynamic_address() { [ -n "$(dynamic_addresses vlan1)" ]; }
no_dynamic_address() { [ -z "$(dynamic_addresses vlan1)" ]; }
no_udhcpc() { [ "$(udhcpc_count)" = 0 ]; }

wait_for_restconf() {
    i=0
    until [ "$(request GET "$IFACES")" = 200 ]; do
        i=$((i + 1))
        [ $i -le 100 ] || { echo "RESTCONF did not come back" >&2; tail -20 /tmp/clixon_backend.log; exit 1; }
        sleep 0.1
    done
}

restart_backend() {
    pkill -x clixon_backend
    while pgrep -x clixon_backend >/dev/null; do sleep 0.1; done
    # What the init script does before every start.
    /usr/local/lib/clixon-switch/prepare-datastore "$XMLDB" "$PERSISTENT" \
        /usr/local/share/clixon-switch/factory-default.xml
    clixon_backend -F -f "$CONFIG" -l e >>/tmp/clixon_backend.log 2>&1 &
    wait_for_restconf
}

echo "# factory default"
check "br-lan filters VLANs" 1 "$(ip -j -d link show br-lan | jq -r '.[0].linkinfo.info_data.vlan_filtering')"
for port in lan1 lan8; do
    check "$port in br-lan" br-lan "$(master $port)"
    check "$port access VLAN 1" "1:PVID,Egress Untagged" "$(vlans $port)"
    check "$port up" true "$(operstate_up $port)"
done
check "br-lan self VLANs" 1 "$(self_vlans)"
check "vlan1 on VLAN 1" 1 "$(ip -j -d link show vlan1 | jq -r '.[0].linkinfo.info_data.id')"
check "vlan1 address" 192.168.1.1/24 "$(addresses vlan1)"
check "no address on br-lan" "" "$(addresses br-lan)"
request GET "$RC/data/clixon-switch:switch" >/dev/null
check "vlan-mode DOT1Q" DOT1Q "$(jq -r '.["clixon-switch:switch"].state["vlan-mode"]' /tmp/body)"

echo "# state data"
request GET "$IFACES/interface=lan1/state" >/dev/null
check "lan1 admin-status" UP "$(jq -r '.["openconfig-interfaces:state"]["admin-status"]' /tmp/body)"
check "lan1 has counters" true "$(jq -r '.["openconfig-interfaces:state"].counters | has("in-octets")' /tmp/body)"

echo "# spanning tree"
# In a network namespace other than the host's, the kernel never leaves
# spanning tree to userspace, and a bridge without it forwards BPDUs instead
# of passing them up to mstpd (CLIXON_SWITCH_STP_IN_NETNS). So the protocol
# cannot converge here: these tests cover mstpd's lifecycle and
# configuration, state data, and the kernel's per-VLAN spanning tree as
# mstpd programs it. Loop protection is tested on the switch.
STP="$RC/data/openconfig-spanning-tree:stp"
# stp_patch <JSON of /stp>: merges into /stp, which may not exist yet
stp_patch() { request PATCH "$RC/data" "{\"ietf-restconf:data\":{\"openconfig-spanning-tree:stp\":$1}}"; }
mstpd_count() { pgrep -xc mstpd || true; }
# unwrap: mstpctl's JSON, one object or an array of them, as one object
unwrap() { jq -c 'if type == "array" then .[0] else . end'; }
vlan_msti() { bridge -j vlan global show dev br-lan | jq -r --argjson v "$1" '[.[0].vlans[] | select(.vlan <= $v and ((.vlanEnd // .vlan) >= $v)) | .msti] | first // "none"'; }
# kernel_msti_state <port> <msti>
kernel_msti_state() { bridge -j mst show dev "$1" | jq -r --argjson m "$2" '[.[0].mst[]? | select(.msti == $m) | .state] | first // "none"'; }
# mstpd_msti_state <port> <msti>, in the kernel's words
mstpd_msti_state() { mstpctl -f json showtreeport br-lan "$1" "$2" | unwrap | jq -r .state | sed 's/discarding/blocking/'; }
msti_states_match() {
    [ "$(kernel_msti_state "$1" "$2")" = "$(mstpd_msti_state "$1" "$2")" ]
}

check "mst_enable on br-lan" 1 "$(ip -j -d link show br-lan | jq -r '.[0].linkinfo.info_data.mst_enabled')"
check "no mstpd while off" 0 "$(mstpd_count)"
status=$(stp_patch '{"global":{"config":{"enabled-protocol":["openconfig-spanning-tree-types:RSTP"],"bpdu-filter":false}},
                     "rstp":{"config":{"bridge-priority":4096,"max-age":24,"forwarding-delay":18}},
                     "interfaces":{"interface":[{"name":"lan2","config":{"name":"lan2","edge-port":"openconfig-spanning-tree-types:EDGE_ENABLE","link-type":"P2P","guard":"ROOT"}}]}}')
check "enable RSTP" 204 "$status"
[ "$status" = 204 ] || cat /tmp/body
check "one mstpd" 1 "$(mstpd_count)"
bridge_json=$(mstpctl -f json showbridge br-lan)
check "mstpd: protocol" rstp "$(echo "$bridge_json" | jq -r '.[0]["force-protocol-version"]')"
check "mstpd: priority, max age, forward delay" "1 24 18" "$(echo "$bridge_json" | jq -r '.[0] | "\(.["bridge-id"][0:1]) \(.["bridge-max-age"]) \(.["bridge-forward-delay"])"')"
check "mstpd: all ports" 8 "$(mstpctl -f json showport br-lan | jq length)"
check "mstpd: lan2 edge, p2p, root guard" "yes no yes yes" "$(mstpctl -f json showportdetail br-lan lan2 | jq -r '.[0] | "\(.["admin-edge-port"]) \(.["auto-edge-port"]) \(.["admin-point-to-point"]) \(.["restricted-role"])"')"
request GET "$STP/rstp/state" >/dev/null
check "state: bridge priority, root priority" "4096 4096" "$(jq -r '.["openconfig-spanning-tree:state"] | "\(.["bridge-priority"]) \(.["designated-root-priority"])"' /tmp/body)"
request GET "$STP/rstp/interfaces/interface=lan2/state" >/dev/null
check "state: lan2 role" openconfig-spanning-tree-types:DESIGNATED "$(jq -r '.["openconfig-spanning-tree:state"].role' /tmp/body)"
request GET "$STP/global/state" >/dev/null
check "state: enabled-protocol" openconfig-spanning-tree-types:RSTP "$(jq -r '.["openconfig-spanning-tree:state"]["enabled-protocol"][0]' /tmp/body)"
rejected "rapid-pvst" PATCH "$RC/data" \
    '{"ietf-restconf:data":{"openconfig-spanning-tree:stp":{"global":{"config":{"enabled-protocol":["openconfig-spanning-tree-types:RAPID_PVST"]}}}}}'
rejected "hello-time 1" PATCH "$STP/rstp/config" '{"openconfig-spanning-tree:config":{"hello-time":1}}'
rejected "loop guard" PATCH "$STP/interfaces/interface=lan2/config" '{"openconfig-spanning-tree:config":{"guard":"LOOP"}}'
check "rejected commits keep mstpd" 1 "$(mstpd_count)"

status=$(request PUT "$STP/global/config" \
    '{"openconfig-spanning-tree:config":{"enabled-protocol":["clixon-switch:STP"]}}')
check "switch to STP" 204 "$status"
check "mstpd: protocol stp" stp "$(mstpctl -f json showbridge br-lan | jq -r '.[0]["force-protocol-version"]')"

echo "# MSTP"
declare_vlans 20 30
for port in lan6 lan7; do
    request PUT "$(switched_vlan $port)" \
        '{"openconfig-vlan:config":{"interface-mode":"TRUNK","native-vlan":1,"trunk-vlans":[20,30]}}' >/dev/null
done
# PUT: a PATCH would add MSTP to the enabled protocols.
status=$(request PUT "$STP/global/config" \
    '{"openconfig-spanning-tree:config":{"enabled-protocol":["openconfig-spanning-tree-types:MSTP"]}}')
check "switch to MSTP" 204 "$status"
config=$(jq -nc '{
    mstp: {config: {name: "lab", revision: 1, "clixon-switch:bridge-priority": 8192},
           "mst-instances": {"mst-instance": [{"mst-id": 2,
               config: {"mst-id": 2, vlan: [20], "bridge-priority": 0},
               interfaces: {interface: [{name: "lan7", config: {name: "lan7", "port-priority": 64}}]}}]}},
    interfaces: {interface: [{name: "lan7", config: {name: "lan7", "edge-port": "openconfig-spanning-tree-types:EDGE_DISABLE"}}]}}')
status=$(stp_patch "$config")
check "MSTI 2" 204 "$status"
[ "$status" = 204 ] || cat /tmp/body
check "mstpd: MSTIs" "0 2" "$(mstpctl -f json showmstilist br-lan | unwrap | jq -r '.mstids | join(" ")')"
check "mstpd: region" "lab 1" "$(mstpctl -f json showmstconfid br-lan | unwrap | jq -r '"\(.["configuration-name"]) \(.["revision-level"])"')"
check "kernel: VLAN 20 in MSTI 2" 2 "$(vlan_msti 20)"
check "kernel: VLAN 30 in the CIST" 0 "$(vlan_msti 30)"
check "kernel: lan6 has a state in MSTI 2" true "$([ "$(kernel_msti_state lan6 2)" != none ] && echo true || echo false)"
check "kernel: lan1 has none in MSTI 2" none "$(kernel_msti_state lan1 2)"
for port in lan6 lan7; do
    check "kernel: $port MSTI 2 state is mstpd's" true "$(wait_until 10 msti_states_match $port 2 && echo true || echo false)"
done
# A VLAN added to an MSTI later: mstpd maps it and sets its states.
status=$(request PATCH "$STP/mstp/mst-instances/mst-instance=2/config" '{"openconfig-spanning-tree:config":{"vlan":[30]}}')
check "VLAN 30 into MSTI 2" 204 "$status"
check "kernel: VLAN 30 in MSTI 2" 2 "$(vlan_msti 30)"
declare_vlans 40
request PUT "$(switched_vlan lan8)" \
    '{"openconfig-vlan:config":{"interface-mode":"TRUNK","native-vlan":1,"trunk-vlans":[20]}}' >/dev/null
check "kernel: lan8 joins MSTI 2 with mstpd's state" true "$(wait_until 10 msti_states_match lan8 2 && echo true || echo false)"
request GET "$STP/mstp/mst-instances/mst-instance=2/state" >/dev/null
check "state: MSTI 2 VLANs, priority" "20 30 0" "$(jq -r '.["openconfig-spanning-tree:state"] | "\(.vlan | map(tostring) | join(" ")) \(.["bridge-priority"])"' /tmp/body)"
request GET "$STP/mstp/state" >/dev/null
check "state: region, CIST priority" "lab 1 8192" "$(jq -r '.["openconfig-spanning-tree:state"] | "\(.name) \(.revision) \(.["clixon-switch:bridge-priority"])"' /tmp/body)"

request DELETE "$STP/mstp/mst-instances" >/dev/null
check "MSTI removed: VLAN 20 back in the CIST" 0 "$(vlan_msti 20)"
check "mstpd: CIST only" 0 "$(mstpctl -f json showmstilist br-lan | unwrap | jq -r '.mstids | join(" ")')"

echo "# spanning tree off"
request PUT "$STP/mstp/mst-instances" \
    '{"openconfig-spanning-tree:mst-instances":{"mst-instance":[{"mst-id":2,"config":{"mst-id":2,"vlan":[20]}}]}}' >/dev/null
check "kernel: VLAN 20 in MSTI 2 again" 2 "$(vlan_msti 20)"
status=$(request DELETE "$STP")
check "delete /stp" 204 "$status"
check "no mstpd" 0 "$(mstpd_count)"
check "kernel: VLAN 20 back in the CIST" 0 "$(vlan_msti 20)"
check "no /stp state" 404 "$(request GET "$STP/global/state")"
# A restarted backend stops the mstpd the old one left behind, when the
# startup configuration has no spanning tree.
stp_patch '{"global":{"config":{"enabled-protocol":["openconfig-spanning-tree-types:RSTP"]}}}' >/dev/null
check "RSTP on again" 1 "$(mstpd_count)"
restart_backend
check "restart: orphaned mstpd stopped" 0 "$(mstpd_count)"
check "restart: lan6 in br-lan" br-lan "$(master lan6)"
for port in lan6 lan7 lan8; do
    request PUT "$(switched_vlan $port)" \
        '{"openconfig-vlan:config":{"interface-mode":"ACCESS","access-vlan":1}}' >/dev/null
done
request DELETE "$VLANS/vlan=20" >/dev/null
request DELETE "$VLANS/vlan=30" >/dev/null
request DELETE "$VLANS/vlan=40" >/dev/null
check "lan7 access VLAN 1 again" "1:PVID,Egress Untagged" "$(vlans lan7)"

echo "# DHCP client"
VLAN1_IPV4_ALL="$IFACES/interface=vlan1/openconfig-vlan:routed-vlan/openconfig-if-ip:ipv4"
# PATCH on ipv4, not ipv4/config: the target of a PATCH must exist.
dhcp_client() { request PATCH "$VLAN1_IPV4_ALL" "{\"openconfig-if-ip:ipv4\":{\"config\":{\"dhcp-client\":$1}}}"; }
status=$(dhcp_client true)
check "enable dhcp-client on vlan1" 204 "$status"
check "one udhcpc" 1 "$(udhcpc_count)"
lease=""
if wait_until 20 has_dynamic_address; then
    lease=$(dynamic_addresses vlan1)
    case "$lease" in
        10.99.0.1[01][0-9]/24) echo "ok   vlan1 leased $lease" ;;
        *) check "vlan1 lease in 10.99.0.100-110/24" "10.99.0.1xx/24" "$lease" ;;
    esac
else
    check "vlan1 gets a DHCP address" "10.99.0.1xx/24" ""
fi
check "static address kept" 192.168.1.1/24 "$(ip -j addr show dev vlan1 | jq -r '[.[0].addr_info[] | select(.family == "inet" and (.dynamic | not)) | "\(.local)/\(.prefixlen)"] | join(" ")')"
check "default route via the DHCP router" "10.99.0.1 vlan1" "$(default_route)"
check "resolv.conf" "search lab.example nameserver 10.99.0.53" "$(tr '\n' ' ' </tmp/resolv.conf 2>/dev/null | sed 's/ $//')"
request GET "$VLAN1_IPV4_ALL" >/dev/null
check "state: dhcp-client" true "$(jq -r '.["openconfig-if-ip:ipv4"].state["dhcp-client"]' /tmp/body)"
check "state: DHCP address" "${lease%/*}" "$(jq -r '[.["openconfig-if-ip:ipv4"].addresses.address[] | select(.state.origin == "DHCP") | .ip] | join(" ")' /tmp/body)"
check "state: static address" 192.168.1.1 "$(jq -r '[.["openconfig-if-ip:ipv4"].addresses.address[] | select(.state.origin == "STATIC") | .ip] | join(" ")' /tmp/body)"
check "state: lease" "10.99.0.1 10.99.0.53 lab.example 600" "$(jq -r '.["openconfig-if-ip:ipv4"].state["clixon-switch:dhcp-lease"] | "\(.router[0]) \(.["dns-server"][0]) \(.domain) \(.["lease-time"])"' /tmp/body)"
check "state: remaining time" true "$(jq -r '.["openconfig-if-ip:ipv4"].state["clixon-switch:dhcp-lease"]["remaining-time"] | . > 0 and . <= 600' /tmp/body)"

request PATCH "$IFACES/interface=lan1/config" '{"openconfig-interfaces:config":{"description":"uplink"}}' >/dev/null
check "unrelated commit keeps the lease" "$lease" "$(dynamic_addresses vlan1)"
check "unrelated commit keeps udhcpc" 1 "$(udhcpc_count)"

declare_vlans 99
rejected "second DHCP client" PUT "$IFACES/interface=vlan99" \
    '{"openconfig-interfaces:interface":[{"name":"vlan99","config":{"name":"vlan99","type":"iana-if-type:l3ipvlan"},"openconfig-vlan:routed-vlan":{"config":{"vlan":99},"openconfig-if-ip:ipv4":{"config":{"dhcp-client":true}}}}]}'
request DELETE "$VLANS/vlan=99" >/dev/null

# A restarted backend stops the udhcpc the old one left behind; the startup
# configuration has no DHCP client.
restart_backend
check "restart: orphaned udhcpc stopped" true "$(wait_until 5 no_udhcpc && echo true || echo false)"
check "restart: lease address gone" true "$(wait_until 5 no_dynamic_address && echo true || echo false)"
check "restart: default route gone" "" "$(default_route)"

dhcp_client true >/dev/null
wait_until 20 has_dynamic_address || true
status=$(dhcp_client false)
check "disable dhcp-client" 204 "$status"
check "no udhcpc" 0 "$(udhcpc_count)"
check "lease address removed" "" "$(dynamic_addresses vlan1)"
check "default route removed" "" "$(default_route)"
check "resolv.conf emptied" "" "$(cat /tmp/resolv.conf)"
request GET "$VLAN1_IPV4_ALL" >/dev/null
check "state: no lease" null "$(jq -r '.["openconfig-if-ip:ipv4"].state["clixon-switch:dhcp-lease"]' /tmp/body)"
request DELETE "$IFACES/interface=lan1/config/description" >/dev/null

echo "# invalid configuration is rejected and not applied"
rejected "unknown port" PUT "$IFACES/interface=lan9" \
    '{"openconfig-interfaces:interface":[{"name":"lan9","config":{"name":"lan9","type":"iana-if-type:ethernetCsmacd"},"openconfig-if-ethernet:ethernet":{"openconfig-vlan:switched-vlan":{"config":{"interface-mode":"ACCESS","access-vlan":1}}}}]}'
rejected "undeclared VLAN" PATCH "$(switched_vlan lan1)" \
    '{"openconfig-vlan:config":{"interface-mode":"ACCESS","access-vlan":20}}'
rejected "unsupported leaf (mtu)" PATCH "$IFACES/interface=lan1/config" \
    '{"openconfig-interfaces:config":{"mtu":1400}}'
check "lan1 still access VLAN 1" "1:PVID,Egress Untagged" "$(vlans lan1)"

echo "# access VLAN change"
declare_vlans 20 30
request PATCH "$IFACES/interface=lan3/openconfig-if-ethernet:ethernet/openconfig-vlan:switched-vlan/config" \
    '{"openconfig-vlan:config":{"interface-mode":"ACCESS","access-vlan":20}}' >/dev/null
check "lan3 access VLAN 20" "20:PVID,Egress Untagged" "$(vlans lan3)"

echo "# management address change"
request PUT "$VLAN1_IPV4/address=10.0.0.2" \
    '{"openconfig-if-ip:address":[{"ip":"10.0.0.2","config":{"ip":"10.0.0.2","prefix-length":8}}]}' >/dev/null
request DELETE "$VLAN1_IPV4/address=192.168.1.1" >/dev/null
check "vlan1 address moved" 10.0.0.2/8 "$(addresses vlan1)"

echo "# unconfigured port leaves the bridge"
request DELETE "$IFACES/interface=lan8" >/dev/null
check "lan8 not in br-lan" none "$(master lan8)"
check "lan8 down" false "$(operstate_up lan8)"

echo "# trunk ports"
request PUT "$(switched_vlan lan2)" \
    '{"openconfig-vlan:config":{"interface-mode":"TRUNK","native-vlan":1,"trunk-vlans":["20..30"]}}' >/dev/null
check "lan2 trunk, native 1, range 20..30" "1:PVID,Egress Untagged 20: 30:" "$(vlans lan2)"
request PUT "$(switched_vlan lan2)" \
    '{"openconfig-vlan:config":{"interface-mode":"TRUNK","native-vlan":30}}' >/dev/null
check "lan2 trunk without list: all VLANs" "1: 20: 30:PVID,Egress Untagged" "$(vlans lan2)"
rejected "undeclared trunk VLAN" PATCH "$(switched_vlan lan2)" \
    '{"openconfig-vlan:config":{"trunk-vlans":[99]}}'
rejected "deleting a VLAN in use" DELETE "$VLANS/vlan=30" ""
check "lan2 unchanged" "1: 20: 30:PVID,Egress Untagged" "$(vlans lan2)"

echo "# suspended VLAN"
request PATCH "$VLANS/vlan=20/config" '{"clixon-switch:config":{"status":"SUSPENDED"}}' >/dev/null
check "lan2 without VLAN 20" "1: 30:PVID,Egress Untagged" "$(vlans lan2)"
check "lan3 without access VLAN 20" "" "$(vlans lan3)"

echo "# routed VLAN by name, VLAN members"
request PATCH "$VLANS/vlan=30/config" '{"clixon-switch:config":{"name":"guests"}}' >/dev/null
request PUT "$IFACES/interface=guests" \
    '{"openconfig-interfaces:interface":[{"name":"guests","config":{"name":"guests","type":"iana-if-type:l3ipvlan"},"openconfig-vlan:routed-vlan":{"config":{"vlan":"guests"},"openconfig-if-ip:ipv4":{"addresses":{"address":[{"ip":"10.30.0.1","config":{"ip":"10.30.0.1","prefix-length":24}}]}}}}]}' >/dev/null
check "guests on VLAN 30" 30 "$(ip -j -d link show guests | jq -r '.[0].linkinfo.info_data.id')"
check "br-lan self VLANs" "1 30" "$(self_vlans)"
request GET "$VLANS/vlan=30" >/dev/null
check "VLAN 30 members" lan2 "$(jq -r '[.["clixon-switch:vlan"][0].members.member[]?.state.interface] | join(" ")' /tmp/body)"

echo "# port-based VLAN groups"
config=$(jq -nc '
    def port(n): {name: n, config: {name: n, type: "iana-if-type:ethernetCsmacd"}};
    def group(id; name; ports): {id: id, config: {id: id, name: name, port: ports}};
    {"ietf-restconf:data": {
     "clixon-switch:switch": {config: {"vlan-mode": "PORT_BASED"}},
     "clixon-switch:port-based-vlans": {group: [
        group(1; "office"; ["lan1", "lan2", "lan3", "lan4"]),
        group(2; "lab"; ["lan5", "lan6", "lan7", "lan8"])]},
     "openconfig-interfaces:interfaces": {interface: ([range(1; 9) | port("lan\(.)")] + [
        {name: "vlan1",
         config: {name: "vlan1", type: "iana-if-type:l3ipvlan"},
         "openconfig-vlan:routed-vlan": {config: {vlan: "office"},
            "openconfig-if-ip:ipv4": {addresses: {address: [
                {ip: "192.168.1.1", config: {ip: "192.168.1.1", "prefix-length": 24}}]}}}}])}}}')
# One PUT replaces the whole configuration: both modes in one commit.
status=$(request PUT "$RC/data" "$config")
check "switch to PORT_BASED" 204 "$status"
[ "$status" = 204 ] || cat /tmp/body
for port in lan1 lan4; do
    check "$port in group 1" "1:PVID,Egress Untagged" "$(vlans $port)"
done
for port in lan5 lan8; do
    check "$port in group 2" "2:PVID,Egress Untagged" "$(vlans $port)"
done
check "br-lan self VLANs" 1 "$(self_vlans)"
check "vlan1 address kept" 192.168.1.1/24 "$(addresses vlan1)"
check "guests removed" false "$(link_exists guests)"
request GET "$RC/data/clixon-switch:port-based-vlans/group=2" >/dev/null
check "group 2 ports" "lan5 lan6 lan7 lan8" "$(jq -r '.["clixon-switch:group"][0].state.port | join(" ")' /tmp/body)"
rejected "port in two groups" PATCH "$RC/data/clixon-switch:port-based-vlans/group=2/config" \
    '{"clixon-switch:config":{"port":["lan1"]}}'
rejected "VLAN database in PORT_BASED mode" PATCH "$VLANS" \
    '{"clixon-switch:vlans":{"vlan":[{"vlan-id":5,"config":{"vlan-id":5}}]}}'

echo "# edits do not touch persistent storage"
check "only startup_db is persistent" startup_db "$(ls "$PERSISTENT")"
check "saved configuration unchanged" 0 "$(grep -c "<access-vlan>20</access-vlan>" "$PERSISTENT/startup_db" || true)"

echo "# restart without save: back to the startup configuration"
restart_backend
check "lan8 in br-lan again" br-lan "$(master lan8)"
check "vlan1 address back" 192.168.1.1/24 "$(addresses vlan1)"
check "lan3 access VLAN 1 again" "1:PVID,Egress Untagged" "$(vlans lan3)"
check "lan5 access VLAN 1 again" "1:PVID,Egress Untagged" "$(vlans lan5)"

echo "# save, then restart: the change persists"
declare_vlans 30
request PATCH "$IFACES/interface=lan3/openconfig-if-ethernet:ethernet/openconfig-vlan:switched-vlan/config" \
    '{"openconfig-vlan:config":{"interface-mode":"ACCESS","access-vlan":30}}' >/dev/null
status=$(request POST "$RC/operations/ietf-netconf:copy-config" \
    '{"ietf-netconf:input":{"target":{"startup":[null]},"source":{"running":[null]}}}')
check "copy-config running to startup" 204 "$status"
check "startup_db is still a symlink" true "$([ -L "$XMLDB/startup_db" ] && echo true || echo false)"
check "saved to persistent storage" 1 "$(grep -c "<access-vlan>30</access-vlan>" "$PERSISTENT/startup_db")"
restart_backend
check "lan3 access VLAN 30 after restart" "30:PVID,Egress Untagged" "$(vlans lan3)"

echo "# broken startup configuration: failsafe factory default"
echo '<config><garbage' > "$PERSISTENT/startup_db"
restart_backend
check "failsafe: lan3 access VLAN 1" "1:PVID,Egress Untagged" "$(vlans lan3)"
check "failsafe: vlan1 address" 192.168.1.1/24 "$(addresses vlan1)"

echo
if [ $failures -gt 0 ]; then
    echo "$failures check(s) failed. Logs: /tmp/clixon_backend.log /tmp/clixon_restconf.log"
    tail -30 /tmp/clixon_backend.log
    exit 1
fi
echo "all checks passed"
