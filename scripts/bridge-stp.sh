#!/bin/sh
# /sbin/bridge-stp: the kernel runs it when spanning tree is switched on
# (start) or off (stop) on a bridge. Success leaves spanning tree to
# userspace; failure makes the kernel run its own STP.
#
# The clixon-switch plugin starts mstpd before it switches spanning tree on,
# and adds and removes the bridge in mstpd itself, so there is nothing to do.
#
#   bridge-stp <bridge> {start|stop}

case "$2" in
    start|stop) exit 0 ;;
    *) echo "usage: $0 <bridge> {start|stop}" >&2; exit 1 ;;
esac
