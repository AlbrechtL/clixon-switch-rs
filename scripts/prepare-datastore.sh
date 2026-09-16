#!/bin/sh
# Prepares clixon's datastores before clixon_backend starts with
# CLICON_STARTUP_MODE startup.
#
# CLICON_XMLDB_DIR is on tmpfs, because clixon rewrites candidate_db and
# running_db on every edit. Flash is written only when the configuration is
# saved:
#
# startup_db   Symlink to <persistent dir>/startup_db, the saved
#              configuration. That file is created from the factory default
#              on first boot, and after a factory reset (deleting it).
# failsafe_db  The factory default of the installed firmware. clixon
#              commits it when startup_db does not commit, so that a broken
#              saved configuration cannot lock the switch out.
#
#   prepare-datastore <CLICON_XMLDB_DIR> <persistent dir> <factory-default.xml>

set -eu

usage="usage: $0 <CLICON_XMLDB_DIR> <persistent dir> <factory-default.xml>"
db=${1:?$usage}
persistent=${2:?$usage}
factory=${3:?$usage}

mkdir -p "$db" "$persistent"

if [ ! -s "$persistent/startup_db" ]; then
    echo "clixon-switch: no saved configuration, starting with the factory default"
    cp "$factory" "$persistent/startup_db.tmp"
    mv "$persistent/startup_db.tmp" "$persistent/startup_db"
fi

ln -sfn "$persistent/startup_db" "$db/startup_db"
cp "$factory" "$db/failsafe_db"
