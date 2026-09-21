#!/bin/sh
# Runs clixon with the plugin in a container, against dummy links lan1..lan7
# and a veth lan8 with a DHCP server behind it, in the container's own network
# namespace.
#
#   dev/container.sh              shell, clixon running (clixon_cli, curl :8080)
#   dev/container.sh <command>    runs <command> instead, e.g.
#                                 python3 -m pytest tests/switch
#
# The repository is mounted read-only; build output lives in a volume.
#
# The image applies the patches of meta-ethernet-switch-os to clixon and
# mstpd. LAYER is the layer to take them from: a local checkout, or by
# default the GitHub repository at LAYER_REV (default master).

set -eu

top=$(cd "$(dirname "$0")/.." && pwd)
image=clixon-switch-dev

layer=${LAYER:-https://github.com/AlbrechtL/meta-ethernet-switch-os.git#${LAYER_REV:-master}}
docker build -q -t "$image" --build-context layer="$layer" "$top/dev" >/dev/null

tty=""
[ -t 0 ] && tty="-it"

exec docker run --rm $tty \
    `# An init that reaps: the tests restart clixon_backend, and whatever` \
    `# runs them is not necessarily waiting for it.` \
    --init \
    --cap-add NET_ADMIN \
    `# mstpd answers mstpctl with the client's SCM_CREDENTIALS attached, which` \
    `# the kernel only allows with CAP_SYS_ADMIN.` \
    --cap-add SYS_ADMIN \
    -v "$top:/src:ro" \
    -v clixon-switch-cargo:/usr/local/cargo/registry \
    -v clixon-switch-target:/target \
    -e CARGO_TARGET_DIR=/target \
    -w /src \
    "$image" dev/in-container.sh "$@"
