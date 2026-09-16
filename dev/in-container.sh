#!/bin/sh
# Inside the dev container: builds and installs the plugin, creates the
# dummy switch ports, starts clixon_backend and clixon_restconf, then runs
# the given command (default: a shell).

set -eu

PORTS="lan1 lan2 lan3 lan4 lan5 lan6 lan7 lan8"
CONFIG=/usr/local/etc/clixon.xml
XMLDB=/usr/local/var/run/clixon-switch
PERSISTENT=/usr/local/var/lib/clixon/clixon-switch

cargo build --locked --release -p clixon-switch-plugin
make -s install BUILDDIR=/tmp/build RESTCONF_PORT=8080 LAN_PORTS="$PORTS" \
    PLUGIN="$CARGO_TARGET_DIR/release/libclixon_switch_plugin.so"
mkdir -p /usr/local/var/run

for port in $PORTS; do
    ip link add "$port" type dummy
done
export CLIXON_SWITCH_PORTS="$PORTS"

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
