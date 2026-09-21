"""What survives a restart.

RESTCONF edits change the running configuration only; the startup database
lives on flash on the switch and is written by an explicit copy-config.
Every test here is marked `restart`, so the fixture puts the startup database
back and starts a cold backend first.
"""

import pytest

from lib import paths
from lib.node import PERSISTENT, XMLDB

pytestmark = pytest.mark.restart

STARTUP_DB = f"{PERSISTENT}/startup_db"
UNTAGGED = {"PVID", "Egress Untagged"}


def set_access_vlan(switch, port: str, vlan: int):
    switch.restconf.ok(
        "PATCH",
        paths.switched_vlan(port),
        {"openconfig-vlan:config": {"interface-mode": "ACCESS", "access-vlan": vlan}},
    )


def save(switch):
    return switch.restconf.ok(
        "POST",
        "operations/ietf-netconf:copy-config",
        {"ietf-netconf:input": {"target": {"startup": [None]}, "source": {"running": [None]}}},
        expect=(204,),
    )


def test_only_the_startup_database_is_persistent(switch):
    assert switch.files.ls(PERSISTENT) == ["startup_db"]


def test_an_edit_does_not_touch_persistent_storage(switch):
    switch.restconf.ok("PATCH", paths.VLANS, paths.vlan_entries(20), expect=(204,))
    set_access_vlan(switch, "lan3", 20)
    assert "<access-vlan>20</access-vlan>" not in switch.files.read(STARTUP_DB)


def test_a_restart_without_saving_returns_to_the_startup_configuration(switch):
    rc = switch.restconf
    rc.ok("PATCH", paths.VLANS, paths.vlan_entries(20), expect=(204,))
    set_access_vlan(switch, "lan3", 20)
    rc.ok("DELETE", paths.interface("lan8"), expect=(204,))
    assert switch.kernel.master("lan8") is None

    switch.restart_backend()
    assert switch.kernel.master("lan8") == "br-lan"
    assert switch.kernel.vlans("lan3") == {1: UNTAGGED}
    assert switch.kernel.addresses("vlan1") == ["192.168.1.1/24"]


def test_a_saved_change_survives_a_restart(switch):
    rc = switch.restconf
    rc.ok("PATCH", paths.VLANS, paths.vlan_entries(30), expect=(204,))
    set_access_vlan(switch, "lan3", 30)
    save(switch)

    assert switch.files.is_symlink(f"{XMLDB}/startup_db"), (
        "the running startup database must stay a symlink into persistent storage"
    )
    assert "<access-vlan>30</access-vlan>" in switch.files.read(STARTUP_DB)

    switch.restart_backend()
    assert switch.kernel.vlans("lan3") == {30: UNTAGGED}


def test_a_broken_startup_configuration_falls_back_to_the_factory_default(switch):
    switch.files.write(STARTUP_DB, "<config><garbage")
    switch.restart_backend()
    assert switch.kernel.vlans("lan3") == {1: UNTAGGED}
    assert switch.kernel.addresses("vlan1") == ["192.168.1.1/24"]
