#!/bin/sh
# Copies the SMIv2 MIB modules the switch's SNMP agent serves, and the import
# closure an NMS needs to load them, from libsmi's MIB directory into mib/.
# They are not installed on the switch (the agent does not parse MIBs); the
# build publishes them next to the firmware, for loading into an NMS.
#
#   scripts/vendor-mibs.sh <libsmi-0.5.0 mibs directory>
#
# Run it in the dev container, like scripts/mib-to-yang.sh: the directory is
# /usr/local/share/libsmi-mibs there, and optional.

set -eu

export LC_ALL=C

# snmpd (net-snmp's mini agent with MIB_MODULES="if-mib agentx", see
# meta-ethernet-switch-os' net-snmp bbappend): the system and snmp groups,
# IF-MIB, and the SNMPv3 engine, MPD, USM and VACM tables.
SNMPD_MIBS="SNMPv2-MIB IF-MIB SNMP-FRAMEWORK-MIB SNMP-MPD-MIB \
    SNMP-USER-BASED-SM-MIB SNMP-VIEW-BASED-ACM-MIB"
# clixon_snmp: the MIBs scripts/mib-to-yang.sh translates.
CLIXON_SNMP_MIBS="BRIDGE-MIB P-BRIDGE-MIB Q-BRIDGE-MIB RSTP-MIB"
LIBSMI_VERSION=0.5.0

src=${1:-/usr/local/share/libsmi-mibs}
top=$(cd "$(dirname "$0")/.." && pwd)
dst=$top/mib

version=$(smidump -V 2>&1 | sed -n 's/^smidump \([0-9.]*\).*/\1/p')
if [ "$version" != "$LIBSMI_VERSION" ]; then
    echo "error: smidump is version ${version:-unknown}, expected $LIBSMI_VERSION" >&2
    exit 1
fi

SMIPATH=$src/ietf:$src/iana:$src/irtf:$src/tubs
export SMIPATH

# Every module in the import tree, the module itself included.
closure() {
    smidump -f imports "$1" | sed -n \
        -e 's/^\([A-Za-z0-9-]\{1,\}\)$/\1/p' \
        -e 's/.*+--\([A-Za-z0-9-]\{1,\}\) .*/\1/p'
}

modules=$(for m in $SNMPD_MIBS $CLIXON_SNMP_MIBS; do closure "$m"; done | sort -u)

rm -rf "$dst"
mkdir -p "$dst"

for m in $modules; do
    path=$(smiquery module "$m" | sed -n 's/^ *Pathname: //p')
    if [ -z "$path" ]; then
        echo "error: $m not found in $src" >&2
        exit 1
    fi
    # net-snmp's naming, which most NMS and snmp tools pick up.
    cp "$path" "$dst/$m.txt"
done

{
    echo "# libsmi $LIBSMI_VERSION mibs, copied by scripts/vendor-mibs.sh"
    echo "# served by snmpd: $(echo $SNMPD_MIBS)"
    echo "# served by clixon_snmp: $CLIXON_SNMP_MIBS"
    echo "# the others are imported by these"
    (cd "$dst" && ls -1 ./*.txt | sed 's|^\./||')
} > "$dst/MANIFEST"
cat "$dst/MANIFEST"
