"""RSTP: what a commit configures in mstpd, and what state data reports.

Every test here needs the kernel to leave spanning tree to mstpd, which the
`spanning_tree` fixture checks.
"""

import pytest

from lib import paths
from lib.wait import until_equal

pytestmark = pytest.mark.usefixtures("spanning_tree")

RSTP_CONFIG = {
    "global": {"config": {"enabled-protocol": [paths.RSTP], "bpdu-filter": False}},
    "rstp": {"config": {"bridge-priority": 4096, "max-age": 24, "forwarding-delay": 18}},
    "interfaces": {
        "interface": [
            {
                "name": "lan2",
                "config": {
                    "name": "lan2",
                    "edge-port": paths.EDGE_ENABLE,
                    "link-type": "P2P",
                    "guard": "ROOT",
                },
            }
        ]
    },
}


@pytest.fixture
def rstp(switch):
    """A switch running RSTP, priority 4096, with lan2 an edge port."""
    switch.restconf.ok("PATCH", paths.DATA, paths.wrap_stp(RSTP_CONFIG), expect=(204,))
    return switch


def test_enabling_rstp_starts_one_mstpd(rstp):
    assert rstp.procs.count("mstpd") == 1


def test_mstpd_runs_the_configured_protocol_and_timers(rstp):
    bridge = rstp.mstpd.bridge()
    assert bridge["force-protocol-version"] == "rstp"
    # The bridge id's first nibble is the priority in units of 4096.
    assert bridge["bridge-id"][:1] == "1"
    assert (bridge["bridge-max-age"], bridge["bridge-forward-delay"]) == (24, 18)


def test_mstpd_manages_every_port(rstp):
    assert len(rstp.mstpd.ports()) == 8


def test_mstpd_has_the_port_features(rstp):
    lan2 = rstp.mstpd.port("lan2")
    assert lan2["admin-edge-port"] == "yes"
    assert lan2["auto-edge-port"] == "no"
    assert lan2["admin-point-to-point"] == "yes"
    assert lan2["restricted-role"] == "yes"


def test_state_reports_the_bridge_as_root(rstp):
    state = rstp.restconf.data(f"{paths.STP}/rstp/state")
    assert state["bridge-priority"] == 4096
    assert state["designated-root-priority"] == 4096


def test_state_reports_the_port_role(rstp):
    state = rstp.restconf.data(f"{paths.STP}/rstp/interfaces/interface=lan2/state")
    assert state["role"] == paths.DESIGNATED


def test_state_reports_the_enabled_protocol(rstp):
    state = rstp.restconf.data(f"{paths.STP}/global/state")
    assert state["enabled-protocol"] == [paths.RSTP]


@pytest.mark.parametrize(
    "what, path, body",
    [
        (
            "rapid-pvst",
            paths.DATA,
            paths.wrap_stp({"global": {"config": {"enabled-protocol": [paths.RAPID_PVST]}}}),
        ),
        (
            "hello-time 1",
            f"{paths.STP}/rstp/config",
            {"openconfig-spanning-tree:config": {"hello-time": 1}},
        ),
        (
            "loop guard",
            f"{paths.STP}/interfaces/interface=lan2/config",
            {"openconfig-spanning-tree:config": {"guard": "LOOP"}},
        ),
    ],
)
def test_unsupported_settings_are_rejected(rstp, what, path, body):
    rstp.restconf.rejected("PATCH", path, body)
    assert rstp.procs.count("mstpd") == 1, "a rejected commit must not disturb mstpd"


def test_switching_to_stp_reconfigures_mstpd(rstp):
    rstp.restconf.ok(
        "PUT",
        f"{paths.STP}/global/config",
        {"openconfig-spanning-tree:config": {"enabled-protocol": [paths.STP_PROTOCOL]}},
        expect=(204,),
    )
    until_equal(
        lambda: rstp.mstpd.bridge().get("force-protocol-version"),
        "stp",
        what="mstpd's protocol version",
    )
