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
addresses() { ip -j addr show dev "$1" 2>/dev/null | jq -r '[.[0].addr_info[]? | select(.family == "inet") | "\(.local)/\(.prefixlen)"] | join(" ")'; }

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

echo "# state data"
request GET "$IFACES/interface=lan1/state" >/dev/null
check "lan1 admin-status" UP "$(jq -r '.["openconfig-interfaces:state"]["admin-status"]' /tmp/body)"
check "lan1 has counters" true "$(jq -r '.["openconfig-interfaces:state"].counters | has("in-octets")' /tmp/body)"

echo "# invalid configuration is rejected and not applied"
rejected "unknown port" PUT "$IFACES/interface=lan9" \
    '{"openconfig-interfaces:interface":[{"name":"lan9","config":{"name":"lan9","type":"iana-if-type:ethernetCsmacd"},"openconfig-if-ethernet:ethernet":{"openconfig-vlan:switched-vlan":{"config":{"interface-mode":"ACCESS","access-vlan":1}}}}]}'
rejected "trunk port" PATCH "$IFACES/interface=lan1/openconfig-if-ethernet:ethernet/openconfig-vlan:switched-vlan/config" \
    '{"openconfig-vlan:config":{"interface-mode":"TRUNK"}}'
rejected "unsupported leaf (mtu)" PATCH "$IFACES/interface=lan1/config" \
    '{"openconfig-interfaces:config":{"mtu":1400}}'
check "lan1 still access VLAN 1" "1:PVID,Egress Untagged" "$(vlans lan1)"

echo "# access VLAN change"
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

echo "# edits do not touch persistent storage"
check "only startup_db is persistent" startup_db "$(ls "$PERSISTENT")"
check "saved configuration unchanged" 0 "$(grep -c "<access-vlan>20</access-vlan>" "$PERSISTENT/startup_db" || true)"

echo "# restart without save: back to the startup configuration"
restart_backend
check "lan8 in br-lan again" br-lan "$(master lan8)"
check "vlan1 address back" 192.168.1.1/24 "$(addresses vlan1)"
check "lan3 access VLAN 1 again" "1:PVID,Egress Untagged" "$(vlans lan3)"

echo "# save, then restart: the change persists"
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
