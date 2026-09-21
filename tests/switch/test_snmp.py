"""SNMPv3, read-only.

snmpd answers the system group and IF-MIB; clixon_snmp answers the bridge
MIBs out of the plugin's state data over AgentX. Only authPriv with SHA and
AES is accepted, and nothing is writable.

The keys below are localized for ENGINE_ID:
    scripts/snmp-localize-key --engine-id 80:00:1f:88:04:74:65:73:74 \
        --auth authpass123 --priv privpass123
"""

import copy

import pytest

from lib import paths
from lib.node import XMLDB
from lib.wait import until, until_equal

ENGINE_ID = "80:00:1f:88:04:74:65:73:74"
AUTH_KEY = "61:27:5e:7f:05:5c:63:11:09:6a:1f:c0:ec:1c:78:1a:cf:5e:23:d8"
PRIV_KEY = "41:f5:c6:d4:a6:dd:41:b7:c0:cb:1c:ef:24:dd:d2:5f"

ENGINE = {
    "enabled": True,
    "engine-id": ENGINE_ID,
    "version": {"v3": [None]},
    "listen": [{"name": "lo", "udp": {"ip": "127.0.0.1", "port": 161}}],
}
ACCESS = {
    "context": "",
    "security-model": "usm",
    "security-level": "auth-priv",
    "read-view": "all",
}

# OIDs, numeric because the client tools load no MIBs.
SYS_CONTACT = "1.3.6.1.2.1.1.4.0"
SYS_NAME = "1.3.6.1.2.1.1.5.0"
SYS_LOCATION = "1.3.6.1.2.1.1.6.0"
SYS_SERVICES = "1.3.6.1.2.1.1.7.0"
IF_NAME = "1.3.6.1.2.1.31.1.1.1.1"
DOT1D_BASE_NUM_PORTS = "1.3.6.1.2.1.17.1.2.0"
DOT1D_BASE_TYPE = "1.3.6.1.2.1.17.1.3.0"
DOT1D_BASE_PORT_IFINDEX = "1.3.6.1.2.1.17.1.4.1.2"
DOT1D_STP_PRIORITY = "1.3.6.1.2.1.17.2.2.0"
DOT1D_STP_ROOT_PORT = "1.3.6.1.2.1.17.2.7.0"
DOT1D_STP_BRIDGE_MAX_AGE = "1.3.6.1.2.1.17.2.12.0"
DOT1D_STP_PORT_STATE = "1.3.6.1.2.1.17.2.15.1.3"
DOT1D_STP_PORT_ENABLE = "1.3.6.1.2.1.17.2.15.1.4"
DOT1D_STP_VERSION = "1.3.6.1.2.1.17.2.16.0"
DOT1D_STP_TX_HOLD_COUNT = "1.3.6.1.2.1.17.2.17.0"
DOT1D_STP_ADMIN_EDGE_PORT = "1.3.6.1.2.1.17.2.19.1.2"
DOT1D_TP_FDB_PORT = "1.3.6.1.2.1.17.4.3.1.2"
DOT1Q_NUM_VLANS = "1.3.6.1.2.1.17.7.1.1.4.0"
DOT1Q_TP_FDB_PORT = "1.3.6.1.2.1.17.7.1.2.2.1.2"
DOT1Q_TP_FDB_STATUS = "1.3.6.1.2.1.17.7.1.2.2.1.3"
DOT1Q_VLAN_CURRENT_UNTAGGED = "1.3.6.1.2.1.17.7.1.4.2.1.5"
DOT1Q_VLAN_STATIC_NAME = "1.3.6.1.2.1.17.7.1.4.3.1.1"
DOT1Q_VLAN_STATIC_EGRESS = "1.3.6.1.2.1.17.7.1.4.3.1.2"
DOT1Q_PVID = "1.3.6.1.2.1.17.7.1.4.5.1.1"
BRIDGE_MIBS = "1.3.6.1.2.1.17"

NOT_WRITABLE = ("notWritable", "noAccess", "noCreation")


def snmp_config(engine=None, access=None, user="nms") -> dict:
    return {
        "ietf-snmp:snmp": {
            "engine": engine if engine is not None else ENGINE,
            "usm": {
                "local": {
                    "user": [
                        {
                            "name": user,
                            "auth": {"sha": {"key": AUTH_KEY}},
                            "priv": {"aes": {"key": PRIV_KEY}},
                        }
                    ]
                }
            },
            "vacm": {
                "group": [
                    {
                        "name": "ro",
                        "member": [{"security-name": user, "security-model": ["usm"]}],
                        "access": [access if access is not None else ACCESS],
                    }
                ],
                "view": [{"name": "all", "include": ["1.3.6.1"]}],
            },
        }
    }


def without(mapping: dict, key: str) -> dict:
    copied = copy.deepcopy(mapping)
    del copied[key]
    return copied


def replacing(mapping: dict, **changes) -> dict:
    copied = copy.deepcopy(mapping)
    copied.update(changes)
    return copied


def mac_oid(mac: str) -> str:
    return "".join(f".{int(octet, 16)}" for octet in mac.split(":"))


@pytest.fixture
def snmp(switch):
    """SNMP enabled with the read-only user `nms`."""
    switch.restconf.ok("PUT", paths.SNMP, snmp_config(), expect=(201,))
    until(lambda: switch.procs.count("snmpd") == 1, what="snmpd to start")
    until(lambda: switch.procs.count("clixon_snmp") == 1, what="clixon_snmp to start")
    until(
        lambda: switch.snmp.get(SYS_NAME, text=True).startswith('"'),
        what="snmpd to answer on the socket",
    )
    # clixon_snmp registers the bridge MIBs over AgentX a moment after it
    # starts, and until it has, they answer "No Such Object".
    until(
        lambda: switch.snmp.get(DOT1D_BASE_NUM_PORTS).isdigit(),
        what="clixon_snmp to register the bridge MIBs",
    )
    return switch


@pytest.fixture
def described(switch):
    """System contact and location, which snmpd reports as sysContact etc."""
    switch.restconf.ok(
        "PATCH",
        paths.DATA,
        {
            "ietf-restconf:data": {
                "clixon-switch:system": {
                    "config": {"contact": "noc@example.com", "location": "rack 3"}
                }
            }
        },
        expect=(204,),
    )
    return switch


def test_no_snmpd_while_snmp_is_off(switch):
    assert switch.procs.count("snmpd") == 0


@pytest.mark.parametrize(
    "what, body",
    [
        (
            "SNMPv2c",
            snmp_config(engine=replacing(ENGINE, version={"v2c": [None], "v3": [None]})),
        ),
        ("no listen address", snmp_config(engine=without(ENGINE, "listen"))),
        ("a write view", snmp_config(access=replacing(ACCESS, **{"write-view": "all"}))),
        (
            "no authentication",
            snmp_config(access=replacing(ACCESS, **{"security-level": "no-auth-no-priv"})),
        ),
    ],
)
def test_weak_configurations_are_rejected(switch, what, body):
    switch.restconf.rejected("PUT", paths.SNMP, body)
    assert switch.procs.count("snmpd") == 0, "a rejected commit must not start snmpd"


def test_a_community_is_rejected(switch):
    body = snmp_config()
    body["ietf-snmp:snmp"]["community"] = [
        {"index": "public", "text-name": "public", "security-name": "nms"}
    ]
    switch.restconf.rejected("PUT", paths.SNMP, body)


@pytest.mark.parametrize(
    "what, auth",
    [
        ("MD5", {"md5": {"key": "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff"}}),
        ("a short key", {"sha": {"key": "00:11"}}),
    ],
)
def test_weak_authentication_is_rejected(switch, what, auth):
    body = snmp_config()
    body["ietf-snmp:snmp"]["usm"]["local"]["user"][0]["auth"] = auth
    switch.restconf.rejected("PUT", paths.SNMP, body)


def test_a_wildcard_view_is_rejected(switch):
    body = snmp_config()
    body["ietf-snmp:snmp"]["vacm"]["view"][0]["include"] = ["1.3.*"]
    switch.restconf.rejected("PUT", paths.SNMP, body)


def test_enabling_starts_one_agent_and_one_subagent(snmp):
    assert snmp.procs.count("snmpd") == 1
    assert snmp.procs.count("clixon_snmp") == 1


def test_the_generated_config_is_readable_by_root_only(snmp):
    assert snmp.files.mode(f"{XMLDB}/snmpd.conf") == "600"


def test_state_reports_the_engine_id_in_use(snmp):
    engine = snmp.restconf.data(f"{paths.SNMP}/engine")
    assert engine["clixon-switch:engine-id-in-use"] == ENGINE_ID


def test_the_system_group(described, snmp):
    assert snmp.snmp.get(SYS_CONTACT, text=True) == '"noc@example.com"'
    assert snmp.snmp.get(SYS_LOCATION, text=True) == '"rack 3"'
    assert snmp.snmp.get(SYS_NAME, text=True) == f'"{snmp.kernel.hostname()}"'
    assert snmp.snmp.get(SYS_SERVICES) == "2"


def test_if_mib_names_the_ports(snmp):
    index = snmp.kernel.ifindex("lan1")
    assert snmp.snmp.get(f"{IF_NAME}.{index}", text=True) == '"lan1"'


def test_version_2c_gets_no_answer(snmp):
    assert not snmp.sh.succeeds(
        ["snmpget", "-v2c", "-c", "public", "-t", "1", "-r", "0", "127.0.0.1", SYS_NAME]
    )


def test_a_wrong_passphrase_gets_no_answer(snmp):
    assert "rack" not in snmp.snmp.get(SYS_LOCATION, auth="wrongpass123")


def test_authnopriv_is_not_authorized(snmp):
    assert "authorizationError" in snmp.snmp.get(SYS_NAME, level="authNoPriv")


@pytest.mark.parametrize(
    "oid, type_, value",
    [(SYS_CONTACT, "s", "hacker"), (DOT1D_STP_PRIORITY, "i", "4096")],
)
def test_nothing_is_writable(snmp, oid, type_, value):
    assert any(error in snmp.snmp.set(oid, type_, value) for error in NOT_WRITABLE)


def test_bridge_mib_base_group(snmp):
    assert snmp.snmp.get(DOT1D_BASE_NUM_PORTS) == "8"
    assert snmp.snmp.get(DOT1D_BASE_TYPE) == "2", "transparent-only"
    assert snmp.snmp.get(f"{DOT1D_BASE_PORT_IFINDEX}.1") == str(snmp.kernel.ifindex("lan1"))


def test_no_spanning_tree_objects_while_it_is_off(snmp):
    assert "No Such" in snmp.snmp.get(DOT1D_STP_PRIORITY)
    assert "No Such" in snmp.snmp.get(DOT1D_STP_VERSION)


def test_q_bridge_mib_follows_commits(snmp):
    rc = snmp.restconf
    rc.ok("PATCH", paths.VLANS, paths.vlan_entries(20), expect=(204,))
    rc.ok(
        "PATCH",
        paths.switched_vlan("lan3"),
        {"openconfig-vlan:config": {"interface-mode": "ACCESS", "access-vlan": 20}},
    )
    rc.ok(
        "PUT",
        f"{paths.VLANS}/vlan=20/config",
        {"clixon-switch:config": {"vlan-id": 20, "name": "office"}},
    )

    assert snmp.snmp.get(f"{DOT1Q_PVID}.3") == "20"
    assert snmp.snmp.get(f"{DOT1Q_VLAN_STATIC_NAME}.20", text=True) == '"office"'
    assert snmp.snmp.get(f"{DOT1Q_VLAN_STATIC_EGRESS}.20").replace(" ", "").strip('"') == "20"
    # Every port but the third is untagged in VLAN 1.
    assert (
        snmp.snmp.get(f"{DOT1Q_VLAN_CURRENT_UNTAGGED}.0.1").replace(" ", "").strip('"') == "DF"
    )
    assert snmp.snmp.get(DOT1Q_NUM_VLANS) == "2"


def test_the_forwarding_database_is_reported(snmp):
    """The DHCP server behind lan8 answers an ARP request, so its address is
    learned on port 8 in VLAN 1."""
    server_mac = snmp.kernel.mac("dhcp-srv")
    snmp.sh.run(
        ["busybox", "ping", "-c", "1", "-W", "1", "-I", "dhcp-srv", "10.99.0.99"],
        check=False,
    )
    oid = mac_oid(server_mac)
    until_equal(
        lambda: snmp.snmp.get(f"{DOT1Q_TP_FDB_PORT}.1{oid}"), "8", what="dot1qTpFdbPort"
    )
    assert snmp.snmp.get(f"{DOT1Q_TP_FDB_STATUS}.1{oid}") == "3", "learned"
    assert snmp.snmp.get(f"{DOT1D_TP_FDB_PORT}{oid}") == "8"


def test_a_walk_of_the_bridge_mibs_ends_cleanly(snmp):
    walked = snmp.snmp.walk(BRIDGE_MIBS)
    assert "OID not increasing" not in walked
    assert "Error" not in walked


def test_a_configuration_change_restarts_snmpd(described, snmp):
    before = snmp.procs.pids("snmpd")
    snmp.restconf.ok(
        "PATCH",
        f"{paths.SYSTEM}/config",
        {"clixon-switch:config": {"location": "rack 4"}},
    )
    until(lambda: snmp.procs.pids("snmpd") != before, what="snmpd to be restarted")
    until_equal(
        lambda: snmp.snmp.get(SYS_LOCATION, text=True), '"rack 4"', what="sysLocation"
    )


def test_a_renamed_user_replaces_the_old_one(snmp):
    """snmpd saves its users in its persistent file; one taken out of the
    configuration must not come back from there."""
    snmp.restconf.ok("PUT", paths.SNMP, snmp_config(user="monitor"), expect=(204,))
    until_equal(
        lambda: snmp.snmp.get(SYS_NAME, text=True, user="monitor"),
        f'"{snmp.kernel.hostname()}"',
        what="the new user's access",
    )
    assert "Unknown user name" in snmp.snmp.get(SYS_NAME)


def test_without_an_engine_id_it_comes_from_the_bridge_mac(switch):
    switch.restconf.ok(
        "PUT", paths.SNMP, snmp_config(engine=without(ENGINE, "engine-id")), expect=(201,)
    )
    engine = switch.restconf.data(f"{paths.SNMP}/engine")
    assert engine["clixon-switch:engine-id-in-use"] == f"80:00:1f:88:03:{switch.kernel.mac('br-lan')}"


def test_deleting_snmp_stops_both_daemons(snmp):
    snmp.restconf.ok("DELETE", paths.SNMP, expect=(204,))
    until_equal(lambda: snmp.procs.count("snmpd"), 0, what="snmpd processes")
    assert snmp.procs.count("clixon_snmp") == 0


@pytest.mark.restart
def test_a_restarted_backend_stops_orphaned_daemons(switch):
    switch.restconf.ok("PUT", paths.SNMP, snmp_config(), expect=(201,))
    until(lambda: switch.procs.count("snmpd") == 1, what="snmpd to start")

    switch.restart_backend()
    until_equal(lambda: switch.procs.count("snmpd"), 0, what="snmpd processes")
    assert switch.procs.count("clixon_snmp") == 0


@pytest.mark.usefixtures("spanning_tree")
def test_spanning_tree_objects_appear_with_rstp(snmp):
    snmp.restconf.ok(
        "PATCH",
        paths.DATA,
        paths.wrap_stp(
            {
                "global": {"config": {"enabled-protocol": [paths.RSTP]}},
                "rstp": {"config": {"bridge-priority": 8192, "hold-count": 4}},
                "interfaces": {
                    "interface": [
                        {
                            "name": "lan2",
                            "config": {"name": "lan2", "edge-port": paths.EDGE_ENABLE},
                        }
                    ]
                },
            }
        ),
        expect=(204,),
    )
    assert snmp.snmp.get(DOT1D_STP_PRIORITY) == "8192"
    assert snmp.snmp.get(DOT1D_STP_BRIDGE_MAX_AGE) == "2000"
    assert snmp.snmp.get(DOT1D_STP_ROOT_PORT) == "0", "the bridge is root"
    assert snmp.snmp.get(f"{DOT1D_STP_PORT_ENABLE}.1") == "1"
    assert snmp.snmp.get(f"{DOT1D_STP_PORT_STATE}.1") in [str(n) for n in range(1, 6)]
    assert snmp.snmp.get(DOT1D_STP_VERSION) == "2", "rstp"
    assert snmp.snmp.get(DOT1D_STP_TX_HOLD_COUNT) == "4"
    assert snmp.snmp.get(f"{DOT1D_STP_ADMIN_EDGE_PORT}.2") == "1"

    snmp.restconf.ok("DELETE", paths.STP, expect=(204,))
    assert "No Such" in snmp.snmp.get(DOT1D_STP_PRIORITY)
