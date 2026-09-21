"""RESTCONF paths, relative to the /restconf root."""

DATA = "data"
IFACES = "data/openconfig-interfaces:interfaces"
VLANS = "data/clixon-switch:vlans"
STP = "data/openconfig-spanning-tree:stp"
SNMP = "data/ietf-snmp:snmp"
SWITCH = "data/clixon-switch:switch"
SYSTEM = "data/clixon-switch:system"
PORT_BASED_VLANS = "data/clixon-switch:port-based-vlans"

VLAN1_IPV4 = f"{IFACES}/interface=vlan1/openconfig-vlan:routed-vlan/openconfig-if-ip:ipv4"
VLAN1_ADDRESSES = f"{VLAN1_IPV4}/addresses"


TYPES = "openconfig-spanning-tree-types"
RSTP = f"{TYPES}:RSTP"
MSTP = f"{TYPES}:MSTP"
RAPID_PVST = f"{TYPES}:RAPID_PVST"
STP_PROTOCOL = "clixon-switch:STP"
EDGE_ENABLE = f"{TYPES}:EDGE_ENABLE"
EDGE_DISABLE = f"{TYPES}:EDGE_DISABLE"
DESIGNATED = f"{TYPES}:DESIGNATED"
ROOT = f"{TYPES}:ROOT"
ALTERNATE = f"{TYPES}:ALTERNATE"
BLOCKING = f"{TYPES}:BLOCKING"
FORWARDING = f"{TYPES}:FORWARDING"


def interface(name: str) -> str:
    return f"{IFACES}/interface={name}"


def switched_vlan(port: str) -> str:
    """The port's VLAN membership: access VLAN or trunk."""
    return (
        f"{interface(port)}/openconfig-if-ethernet:ethernet"
        f"/openconfig-vlan:switched-vlan/config"
    )


def stp_config(subtree: str) -> str:
    return f"{STP}/{subtree}"


def wrap_stp(config: dict) -> dict:
    """A body for PATCHing /stp itself, which need not exist yet."""
    return {"ietf-restconf:data": {"openconfig-spanning-tree:stp": config}}


def vlan_entries(*ids: int) -> dict:
    """A body declaring VLANs in the VLAN database."""
    return {
        "clixon-switch:vlans": {
            "vlan": [{"vlan-id": i, "config": {"vlan-id": i}} for i in ids]
        }
    }
