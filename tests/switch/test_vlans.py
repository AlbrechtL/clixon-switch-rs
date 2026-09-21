"""802.1Q mode: access ports, trunks, the VLAN database and routed VLANs."""

import pytest

from lib import paths

UNTAGGED = {"PVID", "Egress Untagged"}


@pytest.fixture
def vlans(switch):
    """VLANs 20 and 30 declared in the VLAN database."""
    switch.restconf.ok("PATCH", paths.VLANS, paths.vlan_entries(20, 30), expect=(204,))
    return switch


@pytest.fixture
def trunked(vlans):
    """lan2 trunks every VLAN with native 30, lan3 is an access port in 20."""
    vlans.restconf.ok(
        "PUT",
        paths.switched_vlan("lan2"),
        {"openconfig-vlan:config": {"interface-mode": "TRUNK", "native-vlan": 30}},
    )
    vlans.restconf.ok(
        "PATCH",
        paths.switched_vlan("lan3"),
        {"openconfig-vlan:config": {"interface-mode": "ACCESS", "access-vlan": 20}},
    )
    return vlans


def test_access_vlan_change(vlans):
    vlans.restconf.ok(
        "PATCH",
        paths.switched_vlan("lan3"),
        {"openconfig-vlan:config": {"interface-mode": "ACCESS", "access-vlan": 20}},
    )
    assert vlans.kernel.vlans("lan3") == {20: UNTAGGED}


def test_management_address_change(switch):
    rc = switch.restconf
    rc.ok(
        "PUT",
        f"{paths.VLAN1_ADDRESSES}/address=10.0.0.2",
        {
            "openconfig-if-ip:address": [
                {"ip": "10.0.0.2", "config": {"ip": "10.0.0.2", "prefix-length": 8}}
            ]
        },
    )
    rc.ok("DELETE", f"{paths.VLAN1_ADDRESSES}/address=192.168.1.1", expect=(204,))
    assert switch.kernel.addresses("vlan1") == ["10.0.0.2/8"]


def test_an_unconfigured_port_leaves_the_bridge(switch):
    switch.restconf.ok("DELETE", paths.interface("lan8"), expect=(204,))
    assert switch.kernel.master("lan8") is None
    assert not switch.kernel.is_up("lan8")


def test_trunk_with_a_vlan_range(vlans):
    vlans.restconf.ok(
        "PUT",
        paths.switched_vlan("lan2"),
        {
            "openconfig-vlan:config": {
                "interface-mode": "TRUNK",
                "native-vlan": 1,
                "trunk-vlans": ["20..30"],
            }
        },
    )
    assert vlans.kernel.vlans("lan2") == {1: UNTAGGED, 20: set(), 30: set()}


def test_trunk_without_a_list_carries_every_vlan(trunked):
    assert trunked.kernel.vlans("lan2") == {1: set(), 20: set(), 30: UNTAGGED}


def test_an_undeclared_trunk_vlan_is_rejected(trunked):
    trunked.restconf.rejected(
        "PATCH",
        paths.switched_vlan("lan2"),
        {"openconfig-vlan:config": {"trunk-vlans": [99]}},
    )
    assert trunked.kernel.vlans("lan2") == {1: set(), 20: set(), 30: UNTAGGED}


def test_a_vlan_in_use_cannot_be_deleted(trunked):
    trunked.restconf.rejected("DELETE", f"{paths.VLANS}/vlan=30")
    assert trunked.kernel.vlans("lan2") == {1: set(), 20: set(), 30: UNTAGGED}


def test_a_suspended_vlan_leaves_the_ports(trunked):
    trunked.restconf.ok(
        "PATCH",
        f"{paths.VLANS}/vlan=20/config",
        {"clixon-switch:config": {"status": "SUSPENDED"}},
    )
    assert trunked.kernel.vlans("lan2") == {1: set(), 30: UNTAGGED}
    assert trunked.kernel.vlans("lan3") == {}


def test_a_routed_vlan_by_name(trunked):
    rc = trunked.restconf
    rc.ok(
        "PATCH",
        f"{paths.VLANS}/vlan=30/config",
        {"clixon-switch:config": {"name": "guests"}},
    )
    rc.ok(
        "PUT",
        paths.interface("guests"),
        {
            "openconfig-interfaces:interface": [
                {
                    "name": "guests",
                    "config": {"name": "guests", "type": "iana-if-type:l3ipvlan"},
                    "openconfig-vlan:routed-vlan": {
                        "config": {"vlan": "guests"},
                        "openconfig-if-ip:ipv4": {
                            "addresses": {
                                "address": [
                                    {
                                        "ip": "10.30.0.1",
                                        "config": {"ip": "10.30.0.1", "prefix-length": 24},
                                    }
                                ]
                            }
                        },
                    },
                }
            ]
        },
    )
    assert trunked.kernel.link_detail("guests")["id"] == 30
    assert trunked.kernel.self_vlans() == [1, 30]


def test_vlan_members_are_reported(trunked):
    vlan = trunked.restconf.data(f"{paths.VLANS}/vlan=30")
    members = [m["state"]["interface"] for m in vlan.get("members", {}).get("member", [])]
    assert members == ["lan2"]
