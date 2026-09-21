"""The configuration the switch starts with, as scripts/factory-default.sh
generates it: every port an access port in VLAN 1, and vlan1 the management
interface."""

import pytest

from lib import paths

ACCESS_VLAN_1 = {1: {"PVID", "Egress Untagged"}}


def test_bridge_filters_vlans(switch):
    assert switch.kernel.link_detail("br-lan")["vlan_filtering"] == 1


def test_bridge_has_per_vlan_spanning_tree(switch):
    """mst_enabled can only be set while no port has VLANs, so the plugin
    sets it when it creates the bridge, whether or not it is ever used."""
    assert switch.kernel.link_detail("br-lan")["mst_enabled"] == 1


def test_no_mstpd_while_spanning_tree_is_off(switch):
    assert switch.procs.count("mstpd") == 0


@pytest.mark.parametrize("port", ["lan1", "lan8"])
def test_port_is_an_access_port_in_vlan_1(switch, port):
    assert switch.kernel.master(port) == "br-lan"
    assert switch.kernel.vlans(port) == ACCESS_VLAN_1
    assert switch.kernel.is_up(port)


def test_bridge_is_a_member_of_vlan_1_only(switch):
    assert switch.kernel.self_vlans() == [1]


def test_vlan1_is_the_management_interface(switch):
    assert switch.kernel.link_detail("vlan1")["id"] == 1
    assert switch.kernel.addresses("vlan1") == ["192.168.1.1/24"]
    assert switch.kernel.addresses("br-lan") == []


def test_vlan_mode_is_dot1q(switch):
    assert switch.restconf.data(paths.SWITCH)["state"]["vlan-mode"] == "DOT1Q"
