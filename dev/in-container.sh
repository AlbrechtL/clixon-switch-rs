#!/bin/sh
# Inside the dev container: builds and installs the plugin, creates the
# switch ports, starts a DHCP server behind lan8, clixon_backend and
# clixon_restconf, then runs the given command (default: a shell). The
# plugin starts snmpd and clixon_snmp once /snmp enables SNMP.

set -eu

PORTS="lan1 lan2 lan3 lan4 lan5 lan6 lan7 lan8"
CONFIG=/usr/local/etc/clixon.xml
XMLDB=/usr/local/var/run/clixon-switch
PERSISTENT=/usr/local/var/lib/clixon/clixon-switch

cargo build --locked --release -p clixon-switch-plugin
make -s install BUILDDIR=/tmp/build RESTCONF_PORT=8080 LAN_PORTS="$PORTS" \
    PLUGIN="$CARGO_TARGET_DIR/release/libclixon_switch_plugin.so"
mkdir -p /usr/local/var/run

# lan1..lan7 are dummy links. lan8 is one end of a veth pair whose other
# end, dhcp-srv, has a DHCP server: 10.99.0.100-110, router 10.99.0.1, DNS
# 10.99.0.53.
for port in $PORTS; do
    [ "$port" = lan8 ] || ip link add "$port" type dummy
done
ip link add lan8 type veth peer name dhcp-srv
ip addr add 10.99.0.1/24 dev dhcp-srv
ip link set dhcp-srv up
cat >/tmp/udhcpd.conf <<EOF
interface dhcp-srv
start 10.99.0.100
end 10.99.0.110
option subnet 255.255.255.0
option router 10.99.0.1
option dns 10.99.0.53
option domain lab.example
option lease 600
lease_file /tmp/udhcpd.leases
EOF
touch /tmp/udhcpd.leases
busybox udhcpd -S /tmp/udhcpd.conf

export CLIXON_SWITCH_PORTS="$PORTS"
# The repository is mounted read-only: keep pytest's bytecode out of it.
export PYTHONPYCACHEPREFIX=/tmp/pycache
# Docker manages /etc/resolv.conf.
export RESOLV_CONF=/tmp/resolv.conf
# snmpd's engineBoots, on flash on the switch.
export CLIXON_SWITCH_SNMP_PERSISTENT_DIR=/usr/local/var/lib/net-snmp

/usr/local/lib/clixon-switch/prepare-datastore "$XMLDB" "$PERSISTENT" \
    /usr/local/share/clixon-switch/factory-default.xml

clixon_backend -F -f "$CONFIG" -l e >/tmp/clixon_backend.log 2>&1 &
i=0
until [ -S /usr/local/var/run/clixon-switch.sock ]; do
    i=$((i + 1))
    if [ $i -gt 100 ] || ! kill -0 $! 2>/dev/null; then
        cat /tmp/clixon_backend.log
        echo "clixon_backend did not start" >&2
        exit 1
    fi
    sleep 0.1
done

clixon_restconf -f "$CONFIG" -l e >/tmp/clixon_restconf.log 2>&1 &
i=0
until curl -fs -o /dev/null http://localhost:8080/restconf/data/openconfig-interfaces:interfaces; do
    i=$((i + 1))
    if [ $i -gt 100 ]; then
        cat /tmp/clixon_restconf.log
        echo "clixon_restconf did not start" >&2
        exit 1
    fi
    sleep 0.1
done

echo "clixon running: logs in /tmp/clixon_*.log, RESTCONF on http://localhost:8080/restconf"
if [ $# -eq 0 ]; then
    exec bash
fi
exec "$@"
