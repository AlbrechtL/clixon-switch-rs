#!/bin/sh
# Entry point of a lab switch container: waits for the switch ports docker
# attaches, writes the startup configuration from the environment (see
# startup-config.sh), starts clixon_backend and clixon_restconf and runs
# until clixon_backend exits.
#
# SWITCH_CONFIG_FILE, if set, is a datastore file used as it is instead.

set -eu

CONFIG=/usr/local/etc/clixon.xml
XMLDB=/usr/local/var/run/clixon-switch
PERSISTENT=/usr/local/var/lib/clixon/clixon-switch
PORTS=${SWITCH_PORTS:-lan1 lan2}

for port in $PORTS; do
    i=0
    until ip link show "$port" >/dev/null 2>&1; do
        i=$((i + 1))
        if [ $i -gt 100 ]; then
            echo "switch port $port does not exist" >&2
            exit 1
        fi
        sleep 0.1
    done
    # Docker addresses every interface it attaches, and may route over it.
    # A switch port has neither.
    ip addr flush dev "$port"
    ip route flush dev "$port"
    ip -6 route flush dev "$port"
done

if [ -n "${SWITCH_CONFIG_FILE:-}" ]; then
    cp "$SWITCH_CONFIG_FILE" /tmp/startup.xml
else
    /lab/startup-config.sh >/tmp/startup.xml
fi

export CLIXON_SWITCH_PORTS="$PORTS"
# Docker manages /etc/resolv.conf.
export RESOLV_CONF=/tmp/resolv.conf
export CLIXON_SWITCH_SNMP_PERSISTENT_DIR=/usr/local/var/lib/net-snmp

mkdir -p /usr/local/var/run
rm -rf "$PERSISTENT"
/usr/local/lib/clixon-switch/prepare-datastore "$XMLDB" "$PERSISTENT" /tmp/startup.xml

clixon_backend -F -f "$CONFIG" -l e >/tmp/clixon_backend.log 2>&1 &
backend=$!
i=0
until [ -S /usr/local/var/run/clixon-switch.sock ]; do
    i=$((i + 1))
    if [ $i -gt 100 ] || ! kill -0 $backend 2>/dev/null; then
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
touch /tmp/ready
wait $backend
