"""Switching spanning tree off, and what a restarted backend does with the
mstpd its predecessor left running."""

import pytest

from lib import paths
from lib.wait import until_equal

pytestmark = pytest.mark.usefixtures("spanning_tree")

RSTP_ON = paths.wrap_stp({"global": {"config": {"enabled-protocol": [paths.RSTP]}}})


@pytest.fixture
def rstp(switch):
    switch.restconf.ok("PATCH", paths.DATA, RSTP_ON, expect=(204,))
    return switch


def test_deleting_stp_stops_mstpd_and_unmaps_the_vlans(switch):
    rc = switch.restconf
    rc.ok("PATCH", paths.VLANS, paths.vlan_entries(20), expect=(204,))
    rc.ok("PATCH", paths.DATA, RSTP_ON, expect=(204,))
    rc.ok(
        "PUT",
        f"{paths.STP}/global/config",
        {"openconfig-spanning-tree:config": {"enabled-protocol": [paths.MSTP]}},
        expect=(204,),
    )
    rc.ok(
        "PUT",
        f"{paths.STP}/mstp/mst-instances",
        {
            "openconfig-spanning-tree:mst-instances": {
                "mst-instance": [{"mst-id": 2, "config": {"mst-id": 2, "vlan": [20]}}]
            }
        },
    )
    assert switch.kernel.vlan_msti(20) == 2

    rc.ok("DELETE", paths.STP, expect=(204,))
    assert switch.procs.count("mstpd") == 0
    assert switch.kernel.vlan_msti(20) == 0
    assert rc.get(f"{paths.STP}/global/state").status == 404


@pytest.mark.restart
def test_a_restarted_backend_stops_an_orphaned_mstpd(switch):
    """The startup configuration has no spanning tree, so the mstpd the old
    backend started must not survive the new one."""
    switch.restconf.ok("PATCH", paths.DATA, RSTP_ON, expect=(204,))
    assert switch.procs.count("mstpd") == 1

    switch.restart_backend()
    until_equal(lambda: switch.procs.count("mstpd"), 0, what="mstpd processes")
    assert switch.kernel.master("lan6") == "br-lan"
