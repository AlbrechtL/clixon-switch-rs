"""Configuration the switch must refuse, and refuse without applying."""

from lib import paths

ACCESS_VLAN_1 = {1: {"PVID", "Egress Untagged"}}


def test_a_port_the_switch_does_not_have(switch):
    switch.restconf.rejected(
        "PUT",
        paths.interface("lan9"),
        {
            "openconfig-interfaces:interface": [
                {
                    "name": "lan9",
                    "config": {"name": "lan9", "type": "iana-if-type:ethernetCsmacd"},
                    "openconfig-if-ethernet:ethernet": {
                        "openconfig-vlan:switched-vlan": {
                            "config": {"interface-mode": "ACCESS", "access-vlan": 1}
                        }
                    },
                }
            ]
        },
    )


def test_a_vlan_not_in_the_database(switch):
    switch.restconf.rejected(
        "PATCH",
        paths.switched_vlan("lan1"),
        {"openconfig-vlan:config": {"interface-mode": "ACCESS", "access-vlan": 20}},
    )


def test_a_leaf_the_switch_does_not_implement(switch):
    switch.restconf.rejected(
        "PATCH",
        f"{paths.interface('lan1')}/config",
        {"openconfig-interfaces:config": {"mtu": 1400}},
    )


def test_a_rejected_commit_changes_nothing(switch):
    switch.restconf.rejected(
        "PATCH",
        paths.switched_vlan("lan1"),
        {"openconfig-vlan:config": {"interface-mode": "ACCESS", "access-vlan": 20}},
    )
    assert switch.kernel.vlans("lan1") == ACCESS_VLAN_1
