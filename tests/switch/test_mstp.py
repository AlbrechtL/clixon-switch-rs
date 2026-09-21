"""MSTP: mapping VLANs to MST instances, and the per-VLAN spanning tree the
patched mstpd programs into the kernel.

Plain mstpd computes MSTI states but applies only the CIST's; the mstpd of
meta-ethernet-switch-os programs the kernel's per-VLAN states too, which is
what the `kernel ... is mstpd's` tests check.
"""

import pytest

from lib import paths
from lib.wait import until, until_equal

pytestmark = pytest.mark.usefixtures("spanning_tree")

TRUNK_20_30 = {
    "openconfig-vlan:config": {
        "interface-mode": "TRUNK",
        "native-vlan": 1,
        "trunk-vlans": [20, 30],
    }
}

REGION = {
    "mstp": {
        "config": {"name": "lab", "revision": 1, "clixon-switch:bridge-priority": 8192},
        "mst-instances": {
            "mst-instance": [
                {
                    "mst-id": 2,
                    "config": {"mst-id": 2, "vlan": [20], "bridge-priority": 0},
                    "interfaces": {
                        "interface": [
                            {"name": "lan7", "config": {"name": "lan7", "port-priority": 64}}
                        ]
                    },
                }
            ]
        },
    },
    "interfaces": {
        "interface": [
            {"name": "lan7", "config": {"name": "lan7", "edge-port": paths.EDGE_DISABLE}}
        ]
    },
}


@pytest.fixture
def mstp(switch):
    """MSTP with region "lab", VLANs 20 and 30 trunked on lan6 and lan7,
    and MSTI 2 carrying VLAN 20."""
    rc = switch.restconf
    rc.ok("PATCH", paths.VLANS, paths.vlan_entries(20, 30), expect=(204,))
    for port in ("lan6", "lan7"):
        rc.ok("PUT", paths.switched_vlan(port), TRUNK_20_30)
    # PUT, not PATCH: a PATCH would add MSTP to the enabled protocols.
    rc.ok(
        "PUT",
        f"{paths.STP}/global/config",
        {"openconfig-spanning-tree:config": {"enabled-protocol": [paths.MSTP]}},
        expect=(204,),
    )
    rc.ok("PATCH", paths.DATA, paths.wrap_stp(REGION), expect=(204,))
    return switch


def states_agree(node, port, msti):
    """mstpd's state for the port in this MSTI is the kernel's."""
    return node.kernel.msti_state(port, msti) == node.mstpd.msti_state(port, msti)


def test_mstpd_knows_the_instances_and_the_region(mstp):
    assert mstp.mstpd.mstis() == [0, 2]
    assert mstp.mstpd.region() == ("lab", 1)


def test_kernel_maps_the_vlan_to_the_instance(mstp):
    assert mstp.kernel.vlan_msti(20) == 2
    assert mstp.kernel.vlan_msti(30) == 0, "VLAN 30 stays in the CIST"


def test_only_member_ports_have_a_state_in_the_instance(mstp):
    assert mstp.kernel.msti_state("lan6", 2) is not None
    assert mstp.kernel.msti_state("lan1", 2) is None


@pytest.mark.parametrize("port", ["lan6", "lan7"])
def test_kernel_state_is_mstpd_state(mstp, port):
    until(
        lambda: states_agree(mstp, port, 2),
        what=f"{port}'s kernel MSTI 2 state to match mstpd's",
    )


def test_a_vlan_added_later_is_mapped(mstp):
    mstp.restconf.ok(
        "PATCH",
        f"{paths.STP}/mstp/mst-instances/mst-instance=2/config",
        {"openconfig-spanning-tree:config": {"vlan": [30]}},
        expect=(204,),
    )
    assert mstp.kernel.vlan_msti(30) == 2


def test_a_port_joining_later_gets_mstpd_state(mstp):
    mstp.restconf.ok("PATCH", paths.VLANS, paths.vlan_entries(40), expect=(204,))
    mstp.restconf.ok(
        "PUT",
        paths.switched_vlan("lan8"),
        {
            "openconfig-vlan:config": {
                "interface-mode": "TRUNK",
                "native-vlan": 1,
                "trunk-vlans": [20],
            }
        },
    )
    until(
        lambda: states_agree(mstp, "lan8", 2),
        what="lan8's kernel MSTI 2 state to match mstpd's",
    )


def test_instance_state_data(mstp):
    state = mstp.restconf.data(f"{paths.STP}/mstp/mst-instances/mst-instance=2/state")
    assert state["vlan"] == [20]
    assert state["bridge-priority"] == 0


def test_region_state_data(mstp):
    state = mstp.restconf.data(f"{paths.STP}/mstp/state")
    assert (state["name"], state["revision"]) == ("lab", 1)
    assert state["clixon-switch:bridge-priority"] == 8192


def test_removing_the_instances_returns_the_vlans_to_the_cist(mstp):
    mstp.restconf.ok("DELETE", f"{paths.STP}/mstp/mst-instances", expect=(204,))
    until_equal(lambda: mstp.kernel.vlan_msti(20), 0, what="VLAN 20's MSTI")
    assert mstp.mstpd.mstis() == [0]
