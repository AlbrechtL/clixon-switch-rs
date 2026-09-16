use std::collections::{BTreeMap, BTreeSet};

use switch_model::{state_xml, DesiredState, InterfaceState, Port, Svi, Vlan, VlanMode};

fn applied() -> DesiredState {
    DesiredState {
        mode: VlanMode::Dot1q,
        vlans: BTreeMap::from([
            (
                1,
                Vlan {
                    name: Some("default".into()),
                    active: true,
                },
            ),
            (
                20,
                Vlan {
                    name: None,
                    active: false,
                },
            ),
        ]),
        ports: BTreeMap::from([
            ("lan1".to_string(), Port::access(1)),
            (
                "lan2".to_string(),
                Port {
                    enabled: true,
                    native_vlan: None,
                    tagged_vlans: BTreeSet::from([1]),
                },
            ),
        ]),
        svis: BTreeMap::from([(
            "vlan1".to_string(),
            Svi {
                enabled: true,
                vlan: 1,
                addresses: BTreeSet::new(),
            },
        )]),
    }
}

fn up(mac: Option<&str>) -> InterfaceState {
    InterfaceState {
        admin_up: true,
        oper_status: "UP",
        mac: mac.map(String::from),
        in_octets: 1000,
        in_pkts: 10,
        out_octets: 2000,
        out_pkts: 20,
    }
}

#[test]
fn ports_and_svis() {
    let states = BTreeMap::from([
        ("lan1".to_string(), up(Some("00:11:22:33:44:55"))),
        ("vlan1".to_string(), up(Some("00:11:22:33:44:66"))),
        ("eth0".to_string(), up(None)),
    ]);
    let xml = state_xml(&applied(), &states);

    assert!(xml.starts_with(r#"<interfaces xmlns="http://openconfig.net/yang/interfaces">"#));
    assert!(xml.contains("<interface><name>lan1</name><state><admin-status>UP</admin-status><oper-status>UP</oper-status>"));
    assert!(xml.contains("<in-octets>1000</in-octets><in-pkts>10</in-pkts><out-octets>2000</out-octets><out-pkts>20</out-pkts>"));
    // The MAC is ethernet state, so only ports carry it.
    assert!(xml.contains("<hw-mac-address>00:11:22:33:44:55</hw-mac-address>"));
    assert!(!xml.contains("00:11:22:33:44:66"));
    assert!(xml.contains("<name>vlan1</name>"));
    // Only configured interfaces.
    assert!(!xml.contains("eth0"));
}

#[test]
fn interfaces_without_state_are_skipped() {
    assert!(state_xml(&applied(), &BTreeMap::new()).starts_with(
        r#"<interfaces xmlns="http://openconfig.net/yang/interfaces"></interfaces><switch "#
    ));
}

#[test]
fn vlans_with_members() {
    let xml = state_xml(&applied(), &BTreeMap::new());
    assert!(xml.contains(
        r#"<switch xmlns="urn:github:albrechtl:clixon-switch"><state><vlan-mode>DOT1Q</vlan-mode></state></switch>"#
    ));
    assert!(xml.contains(r#"<vlans xmlns="urn:github:albrechtl:clixon-switch">"#));
    // Tagged and untagged members alike.
    assert!(xml.contains(
        "<vlan><vlan-id>1</vlan-id><state><vlan-id>1</vlan-id><name>default</name><status>ACTIVE</status></state>\
         <members><member><state><interface>lan1</interface></state></member>\
         <member><state><interface>lan2</interface></state></member></members></vlan>"
    ));
    assert!(xml.contains(
        "<vlan><vlan-id>20</vlan-id><state><vlan-id>20</vlan-id><status>SUSPENDED</status></state><members></members></vlan>"
    ));
    assert!(!xml.contains("port-based-vlans"));
}

#[test]
fn port_based_groups() {
    let mut applied = applied();
    applied.mode = VlanMode::PortBased;
    applied.vlans = BTreeMap::from([(
        1,
        Vlan {
            name: Some("office".into()),
            active: true,
        },
    )]);
    applied.ports.insert("lan2".into(), Port::access(1));
    let xml = state_xml(&applied, &BTreeMap::new());
    assert!(xml.contains("<vlan-mode>PORT_BASED</vlan-mode>"));
    assert!(xml.contains(
        r#"<port-based-vlans xmlns="urn:github:albrechtl:clixon-switch"><group><id>1</id><state><id>1</id><name>office</name><port>lan1</port><port>lan2</port></state></group></port-based-vlans>"#
    ));
    assert!(!xml.contains("<vlans "));
}
