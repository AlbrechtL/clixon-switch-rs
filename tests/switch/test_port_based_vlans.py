"""Port-based VLAN mode: groups of ports that may only talk among themselves.

One PUT of the whole configuration switches mode and defines the groups in a
single commit, which is the only way to change `vlan-mode`.
"""

import pytest

from lib import paths

UNTAGGED = {"PVID", "Egress Untagged"}


def port(name: str) -> dict:
    return {"name": name, "config": {"name": name, "type": "iana-if-type:ethernetCsmacd"}}


def group(id_: int, name: str, ports: list[str]) -> dict:
    return {"id": id_, "config": {"id": id_, "name": name, "port": ports}}


PORT_BASED = {
    "ietf-restconf:data": {
        "clixon-switch:switch": {"config": {"vlan-mode": "PORT_BASED"}},
        "clixon-switch:port-based-vlans": {
            "group": [
                group(1, "office", ["lan1", "lan2", "lan3", "lan4"]),
                group(2, "lab", ["lan5", "lan6", "lan7", "lan8"]),
            ]
        },
        "openconfig-interfaces:interfaces": {
            "interface": [port(f"lan{n}") for n in range(1, 9)]
            + [
                {
                    "name": "vlan1",
                    "config": {"name": "vlan1", "type": "iana-if-type:l3ipvlan"},
                    "openconfig-vlan:routed-vlan": {
                        "config": {"vlan": "office"},
                        "openconfig-if-ip:ipv4": {
                            "addresses": {
                                "address": [
                                    {
                                        "ip": "192.168.1.1",
                                        "config": {
                                            "ip": "192.168.1.1",
                                            "prefix-length": 24,
                                        },
                                    }
                                ]
                            }
                        },
                    },
                }
            ]
        },
    }
}


@pytest.fixture
def port_based(switch):
    switch.restconf.ok("PUT", paths.DATA, PORT_BASED, expect=(204,))
    return switch


@pytest.mark.parametrize("name, vlan", [("lan1", 1), ("lan4", 1), ("lan5", 2), ("lan8", 2)])
def test_each_port_is_untagged_in_its_group(port_based, name, vlan):
    assert port_based.kernel.vlans(name) == {vlan: UNTAGGED}


def test_the_bridge_routes_only_the_management_group(port_based):
    assert port_based.kernel.self_vlans() == [1]
    assert port_based.kernel.addresses("vlan1") == ["192.168.1.1/24"]


def test_switching_mode_removes_a_routed_vlan_of_the_old_mode(switch):
    rc = switch.restconf
    rc.ok("PATCH", paths.VLANS, paths.vlan_entries(30), expect=(204,))
    rc.ok(
        "PUT",
        paths.interface("guests"),
        {
            "openconfig-interfaces:interface": [
                {
                    "name": "guests",
                    "config": {"name": "guests", "type": "iana-if-type:l3ipvlan"},
                    "openconfig-vlan:routed-vlan": {"config": {"vlan": 30}},
                }
            ]
        },
    )
    assert switch.kernel.exists("guests")

    rc.ok("PUT", paths.DATA, PORT_BASED, expect=(204,))
    assert not switch.kernel.exists("guests")


def test_group_state_lists_its_ports(port_based):
    state = port_based.restconf.data(f"{paths.PORT_BASED_VLANS}/group=2")["state"]
    assert state["port"] == ["lan5", "lan6", "lan7", "lan8"]


def test_a_port_may_be_in_one_group_only(port_based):
    port_based.restconf.rejected(
        "PATCH",
        f"{paths.PORT_BASED_VLANS}/group=2/config",
        {"clixon-switch:config": {"port": ["lan1"]}},
    )


def test_the_vlan_database_is_refused_in_this_mode(port_based):
    port_based.restconf.rejected(
        "PATCH",
        paths.VLANS,
        {"clixon-switch:vlans": {"vlan": [{"vlan-id": 5, "config": {"vlan-id": 5}}]}},
    )
