#!/bin/sh
# Runs the plugin from "devtool build clixon-switch" on a switch, from RAM:
# nothing is written to flash, and a reboot returns to the installed version.
#
#   . oe-init-build-env        # sets BUILDDIR
#   devtool build clixon-switch
#   scripts/deploy.sh [root@192.168.1.1]
#
# Copies the plugin to /tmp/clixon-switch on the switch, stops the installed
# clixon_backend and runs one in the foreground that loads the copy, with
# its log on this terminal. Ctrl-C stops it; "/etc/init.d/clixon-backend
# start" on the switch brings back the installed plugin.
#
# Only the plugin is replaced. Changes to YANG, clixon.xml or the CLI spec
# need an image (or devtool deploy-target, which writes to flash).

set -eu

target=${1:-root@192.168.1.1}

: "${BUILDDIR:?source oe-init-build-env first}"
plugin=$(ls -t "$BUILDDIR"/tmp/work/*/clixon-switch/*/build/target/*/release/libclixon_switch_plugin.so 2>/dev/null | head -n 1)
if [ -z "$plugin" ]; then
    echo "no plugin in $BUILDDIR/tmp/work, run devtool build clixon-switch first" >&2
    exit 1
fi
echo "deploying $plugin"

ssh "$target" 'mkdir -p /tmp/clixon-switch/backend'
scp -q "$plugin" "$target:/tmp/clixon-switch/backend/clixon-switch_backend.so"

# -s running keeps the configuration the installed backend was running.
exec ssh -t "$target" '
    /etc/init.d/clixon-backend stop
    exec clixon_backend -F -l e -s running -o CLICON_BACKEND_DIR=/tmp/clixon-switch/backend
'
