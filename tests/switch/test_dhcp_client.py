"""The DHCP client on the management interface.

dev/in-container.sh puts a busybox DHCP server behind lan8, leasing
10.99.0.100-110 with router 10.99.0.1, DNS 10.99.0.53 and domain lab.example.
"""

import ipaddress

import pytest

from lib import paths
from lib.wait import until, until_equal

RESOLV_CONF = "/tmp/resolv.conf"
POOL = [ipaddress.ip_address(f"10.99.0.{n}") for n in range(100, 111)]


def set_dhcp_client(switch, enabled: bool):
    # PATCH on ipv4, not ipv4/config: the target of a PATCH must exist.
    return switch.restconf.ok(
        "PATCH",
        paths.VLAN1_IPV4,
        {"openconfig-if-ip:ipv4": {"config": {"dhcp-client": enabled}}},
        expect=(204,),
    )


def leased(switch) -> list[str]:
    return switch.kernel.addresses("vlan1", dynamic=True)


@pytest.fixture
def dhcp(switch):
    """vlan1 with a lease from the container's DHCP server."""
    set_dhcp_client(switch, True)
    until(lambda: leased(switch), timeout=20, what="vlan1 to get a DHCP lease")
    return switch


def test_enabling_starts_one_udhcpc(switch):
    set_dhcp_client(switch, True)
    assert switch.procs.count("udhcpc") == 1


def test_the_lease_comes_from_the_pool(dhcp):
    addresses = leased(dhcp)
    assert len(addresses) == 1
    address = ipaddress.ip_interface(addresses[0])
    assert address.network.prefixlen == 24
    assert address.ip in POOL


def test_the_static_address_is_kept(dhcp):
    assert dhcp.kernel.addresses("vlan1", dynamic=False) == ["192.168.1.1/24"]


def test_the_default_route_and_resolver_come_from_the_lease(dhcp):
    assert dhcp.kernel.default_routes() == [("10.99.0.1", "vlan1")]
    resolv = dhcp.files.read(RESOLV_CONF).split()
    assert resolv == ["search", "lab.example", "nameserver", "10.99.0.53"]


def test_state_reports_both_addresses_and_the_lease(dhcp):
    ipv4 = dhcp.restconf.data(paths.VLAN1_IPV4)
    assert ipv4["state"]["dhcp-client"] is True

    origins = {a["state"]["origin"]: a["ip"] for a in ipv4["addresses"]["address"]}
    assert origins["DHCP"] == leased(dhcp)[0].split("/")[0]
    assert origins["STATIC"] == "192.168.1.1"

    lease = ipv4["state"]["clixon-switch:dhcp-lease"]
    assert lease["router"] == ["10.99.0.1"]
    assert lease["dns-server"] == ["10.99.0.53"]
    assert lease["domain"] == "lab.example"
    assert lease["lease-time"] == 600
    assert 0 < lease["remaining-time"] <= 600


def test_an_unrelated_commit_keeps_the_lease(dhcp):
    before = leased(dhcp)
    dhcp.restconf.ok(
        "PATCH",
        f"{paths.interface('lan1')}/config",
        {"openconfig-interfaces:config": {"description": "uplink"}},
    )
    assert leased(dhcp) == before
    assert dhcp.procs.count("udhcpc") == 1


def test_only_one_interface_may_run_a_dhcp_client(dhcp):
    dhcp.restconf.ok("PATCH", paths.VLANS, paths.vlan_entries(99), expect=(204,))
    dhcp.restconf.rejected(
        "PUT",
        paths.interface("vlan99"),
        {
            "openconfig-interfaces:interface": [
                {
                    "name": "vlan99",
                    "config": {"name": "vlan99", "type": "iana-if-type:l3ipvlan"},
                    "openconfig-vlan:routed-vlan": {
                        "config": {"vlan": 99},
                        "openconfig-if-ip:ipv4": {"config": {"dhcp-client": True}},
                    },
                }
            ]
        },
    )


def test_disabling_removes_the_lease(dhcp):
    set_dhcp_client(dhcp, False)
    assert dhcp.procs.count("udhcpc") == 0
    assert leased(dhcp) == []
    assert dhcp.kernel.default_routes() == []
    assert dhcp.files.read(RESOLV_CONF) == ""
    ipv4 = dhcp.restconf.data(paths.VLAN1_IPV4)
    assert "clixon-switch:dhcp-lease" not in ipv4["state"]


@pytest.mark.restart
def test_a_restarted_backend_stops_an_orphaned_udhcpc(switch):
    """The startup configuration has no DHCP client."""
    set_dhcp_client(switch, True)
    until(lambda: leased(switch), timeout=20, what="vlan1 to get a DHCP lease")

    switch.restart_backend()
    until_equal(lambda: switch.procs.count("udhcpc"), 0, timeout=5, what="udhcpc processes")
    until_equal(lambda: leased(switch), [], timeout=5, what="vlan1's dynamic addresses")
    assert switch.kernel.default_routes() == []
