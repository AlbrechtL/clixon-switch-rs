#!/bin/sh
# Called by udhcpc, which the clixon-switch backend plugin runs on the one
# routed VLAN interface with ipv4/config/dhcp-client true:
#
#   udhcpc -f -S -R -i <interface> -s <this script> -p <pid file> ...
#
# udhcpc passes the event as $1 and the lease in the environment ($interface,
# $ip, $mask, $router, $dns, $domain, $serverid, $lease). The plugin passes
# CLIXON_SWITCH_RUNDIR and RESOLV_CONF.
#
# The leased address gets the lease time as its lifetime. The kernel marks it
# "dynamic" (no IFA_F_PERMANENT), which is how the plugin tells it from the
# static addresses it manages, and expires it should udhcpc die. Static
# addresses are never changed here, even when one equals the lease.
#
# The lease is recorded in $CLIXON_SWITCH_RUNDIR/lease.<interface>, for the
# plugin's state data.

run_dir=${CLIXON_SWITCH_RUNDIR:?set by the clixon-switch plugin}
resolv_conf=${RESOLV_CONF:-/etc/resolv.conf}
lease_file=$run_dir/lease.$interface

# address_kind <address/prefix-length>: "dynamic", "permanent" or nothing.
address_kind() {
    ip -o -4 addr show dev "$interface" 2>/dev/null | while read -r line; do
        case "$line" in
            *" inet $1 "*dynamic*) echo dynamic ;;
            *" inet $1 "*) echo permanent ;;
        esac
    done
}

delete_dynamic() {
    [ "$(address_kind "$1")" = dynamic ] && ip -4 addr del "$1" dev "$interface"
}

# write <file>: stdin to <file>, atomically. A symlink (/etc/resolv.conf into
# tmpfs) stays a symlink: the file it points to is replaced.
write() {
    target=$(readlink -f "$1" 2>/dev/null) || target=$1
    cat >"$target.tmp" && mv -f "$target.tmp" "$target"
}

# The address of the recorded lease, e.g. 10.0.0.2/24.
recorded_address() {
    [ -f "$lease_file" ] || return 0
    recorded_ip=$(sed -n 's/^ip=//p' "$lease_file")
    recorded_mask=$(sed -n 's/^mask=//p' "$lease_file")
    [ -n "$recorded_ip" ] && echo "$recorded_ip/$recorded_mask"
}

case "$1" in
    deconfig)
        old=$(recorded_address)
        [ -n "$old" ] && delete_dynamic "$old"
        while ip -4 route del default dev "$interface" 2>/dev/null; do :; done
        rm -f "$lease_file"
        : | write "$resolv_conf"
        ;;

    bound|renew)
        old=$(recorded_address)
        new=$ip/$mask
        if [ "$(address_kind "$new")" != permanent ]; then
            ip -4 addr replace "$new" dev "$interface" \
                valid_lft "$lease" preferred_lft "$lease" ${broadcast:+broadcast "$broadcast"}
        fi
        [ -n "$old" ] && [ "$old" != "$new" ] && delete_dynamic "$old"

        # The first router is the default gateway.
        set -- $router
        if [ -n "$1" ]; then
            ip -4 route replace default via "$1" dev "$interface"
        else
            while ip -4 route del default dev "$interface" 2>/dev/null; do :; done
        fi

        {
            [ -n "$domain" ] && echo "search $domain"
            for server in $dns; do
                echo "nameserver $server"
            done
        } | write "$resolv_conf"

        write "$lease_file" <<EOF
ip=$ip
mask=$mask
router=$router
dns=$dns
domain=$domain
serverid=$serverid
lease=$lease
EOF
        ;;
esac

exit 0
