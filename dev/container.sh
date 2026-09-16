#!/bin/sh
# Runs clixon with the plugin in a container, against dummy links lan1..lan8
# in the container's own network namespace.
#
#   dev/container.sh              shell, clixon running (clixon_cli, curl :8080)
#   dev/container.sh <command>    runs <command> instead, e.g. tests/integration/run.sh
#
# The repository is mounted read-only; build output lives in a volume.

set -eu

top=$(cd "$(dirname "$0")/.." && pwd)
image=clixon-switch-dev

docker build -q -t "$image" "$top/dev" >/dev/null

tty=""
[ -t 0 ] && tty="-it"

exec docker run --rm $tty \
    --cap-add NET_ADMIN \
    -v "$top:/src:ro" \
    -v clixon-switch-cargo:/usr/local/cargo/registry \
    -v clixon-switch-target:/target \
    -e CARGO_TARGET_DIR=/target \
    -w /src \
    "$image" dev/in-container.sh "$@"
