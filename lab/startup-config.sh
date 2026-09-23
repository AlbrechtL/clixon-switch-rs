#!/bin/sh
# Prints the startup configuration of one lab switch as a clixon datastore,
# from the environment:
#
#   SWITCH_PORTS         switch ports, access ports in VLAN 1 (lan1 lan2)
#   SWITCH_ADDRESS       address of vlan1 (192.168.1.1/24)
#   STP_PROTOCOL         RSTP, MSTP or STP; empty: spanning tree off
#   STP_BRIDGE_PRIORITY  bridge priority (32768)
#   STP_MAX_AGE          max-age, if set
#   STP_FORWARDING_DELAY forwarding-delay, if set
#   STP_EDGE_PORTS       ports with edge-port EDGE_ENABLE
#
# Spanning tree has to be in the startup configuration: in a ring, ports
# that join br-lan before spanning tree runs make a broadcast storm.

set -eu

ports=${SWITCH_PORTS:-lan1 lan2}
address=${SWITCH_ADDRESS:-192.168.1.1/24}
protocol=${STP_PROTOCOL:-}
priority=${STP_BRIDGE_PRIORITY:-32768}
edge_ports=${STP_EDGE_PORTS:-}
timers=""
[ -z "${STP_MAX_AGE:-}" ] || timers="$timers<max-age>$STP_MAX_AGE</max-age>"
[ -z "${STP_FORWARDING_DELAY:-}" ] || timers="$timers<forwarding-delay>$STP_FORWARDING_DELAY</forwarding-delay>"

# The factory default without its closing </config>. factory-default.sh
# is next to this script in the image, in scripts/ in the repository.
here=$(dirname "$0")
factory=$here/factory-default.sh
[ -f "$factory" ] || factory=$here/../scripts/factory-default.sh
sh "$factory" "$ports" "$address" | sed '$d'

if [ -n "$protocol" ]; then
    types='xmlns:oc-stp-types="http://openconfig.net/yang/spanning-tree/types"'
    case $protocol in
        RSTP|MSTP) identity="oc-stp-types:$protocol" ;;
        STP) identity="sw:STP"; types="$types xmlns:sw=\"urn:github:albrechtl:clixon-switch\"" ;;
        *) echo "STP_PROTOCOL: RSTP, MSTP or STP, not $protocol" >&2; exit 1 ;;
    esac
    if [ "$protocol" = MSTP ]; then
        tree="<mstp><config>$timers<bridge-priority xmlns=\"urn:github:albrechtl:clixon-switch\">$priority</bridge-priority></config></mstp>"
    else
        tree="<rstp><config>$timers<bridge-priority>$priority</bridge-priority></config></rstp>"
    fi
    cat <<XML
  <stp xmlns="http://openconfig.net/yang/spanning-tree">
    <global>
      <config>
        <enabled-protocol $types>$identity</enabled-protocol>
      </config>
    </global>
    $tree
XML
    if [ -n "$edge_ports" ]; then
        echo "    <interfaces>"
        for port in $edge_ports; do
            cat <<XML
      <interface>
        <name>$port</name>
        <config>
          <name>$port</name>
          <edge-port $types>oc-stp-types:EDGE_ENABLE</edge-port>
        </config>
      </interface>
XML
        done
        echo "    </interfaces>"
    fi
    echo "  </stp>"
fi

echo "</config>"
