#!/bin/sh
# Builds the dev image (dev/Dockerfile, as dev/container.sh does) and the
# lab image on top (lab/Dockerfile), then starts the lab and waits until every switch is up.
#
#   lab/up.sh                          compose.yaml next to this script
#   lab/up.sh -f other.yaml            another topology
#
# LAYER / LAYER_REV: where the clixon and mstpd patches come from, as for
# dev/container.sh. Tear down with: docker compose -f lab/compose.yaml down

set -eu

top=$(cd "$(dirname "$0")/.." && pwd)

# Spanning tree in a container needs the bridge's stp_mode (Linux 7.1).
release=$(uname -r)
major=${release%%.*}
minor=${release#*.}
minor=${minor%%[!0-9]*}
if [ "$major" -lt 7 ] || { [ "$major" -eq 7 ] && [ "$minor" -lt 1 ]; }; then
    echo "warning: Linux $release has no bridge stp_mode (7.1): spanning tree" \
         "will not start in the containers" >&2
fi

layer=${LAYER:-https://github.com/AlbrechtL/meta-ethernet-switch-os.git#${LAYER_REV:-master}}
docker build -q -t clixon-switch-dev --build-context layer="$layer" "$top/dev" >/dev/null
docker build -q -t clixon-switch-lab -f "$top/lab/Dockerfile" "$top" >/dev/null

[ $# -gt 0 ] || set -- -f "$top/lab/compose.yaml"
exec docker compose "$@" up -d --force-recreate --wait
