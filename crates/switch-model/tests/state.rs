use std::collections::{BTreeMap, BTreeSet};

use std::net::Ipv4Addr;

use switch_model::{
    state_xml, AddressOrigin, DesiredState, DhcpLease, InterfaceState, Ipv4Prefix, Port, Svi, Vlan,
    VlanMode,
};

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
                dhcp_client: false,
            },
        )]),
        stp: None,
        ..DesiredState::default()
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
        ..InterfaceState::default()
    }
}

#[test]
fn ports_and_svis() {
    let states = BTreeMap::from([
        ("lan1".to_string(), up(Some("00:11:22:33:44:55"))),
        ("vlan1".to_string(), up(Some("00:11:22:33:44:66"))),
        ("eth0".to_string(), up(None)),
    ]);
    let xml = state_xml(&applied(), &states, None);

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
    assert!(state_xml(&applied(), &BTreeMap::new(), None).starts_with(
        r#"<interfaces xmlns="http://openconfig.net/yang/interfaces"></interfaces><switch "#
    ));
}

#[test]
fn vlans_with_members() {
    let xml = state_xml(&applied(), &BTreeMap::new(), None);
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
    let xml = state_xml(&applied, &BTreeMap::new(), None);
    assert!(xml.contains("<vlan-mode>PORT_BASED</vlan-mode>"));
    assert!(xml.contains(
        r#"<port-based-vlans xmlns="urn:github:albrechtl:clixon-switch"><group><id>1</id><state><id>1</id><name>office</name><port>lan1</port><port>lan2</port></state></group></port-based-vlans>"#
    ));
    assert!(!xml.contains("<vlans "));
}

fn prefix(addr: [u8; 4], prefix_len: u8) -> Ipv4Prefix {
    Ipv4Prefix {
        addr: Ipv4Addr::from(addr),
        prefix_len,
    }
}

#[test]
fn svi_addresses_and_dhcp_lease() {
    let mut applied = applied();
    applied.svis.get_mut("vlan1").unwrap().dhcp_client = true;
    let lease = DhcpLease {
        address: Some(prefix([10, 99, 0, 100], 24)),
        routers: vec![Ipv4Addr::new(10, 99, 0, 1)],
        dns_servers: vec![Ipv4Addr::new(10, 99, 0, 53), Ipv4Addr::new(10, 99, 0, 54)],
        domain: Some("lab&co".into()),
        server: Some(Ipv4Addr::new(10, 99, 0, 1)),
        lease_time: Some(3600),
        remaining_time: Some(3599),
    };
    let vlan1 = InterfaceState {
        addresses: BTreeMap::from([
            (prefix([192, 168, 1, 1], 24), AddressOrigin::Static),
            (prefix([10, 99, 0, 100], 24), AddressOrigin::Dhcp),
        ]),
        dhcp_lease: Some(lease),
        ..up(None)
    };
    let xml = state_xml(
        &applied,
        &BTreeMap::from([("vlan1".to_string(), vlan1)]),
        None,
    );

    assert!(xml.contains(
        r#"<routed-vlan xmlns="http://openconfig.net/yang/vlan"><ipv4 xmlns="http://openconfig.net/yang/interfaces/ip"><addresses>"#
    ));
    assert!(xml.contains(
        "<address><ip>10.99.0.100</ip><state><ip>10.99.0.100</ip><prefix-length>24</prefix-length><origin>DHCP</origin></state></address>"
    ));
    assert!(xml.contains(
        "<address><ip>192.168.1.1</ip><state><ip>192.168.1.1</ip><prefix-length>24</prefix-length><origin>STATIC</origin></state></address>"
    ));
    assert!(xml.contains(
        r#"</addresses><state><dhcp-client>true</dhcp-client><dhcp-lease xmlns="urn:github:albrechtl:clixon-switch"><address>10.99.0.100</address><prefix-length>24</prefix-length><router>10.99.0.1</router><dns-server>10.99.0.53</dns-server><dns-server>10.99.0.54</dns-server><domain>lab&amp;co</domain><server>10.99.0.1</server><lease-time>3600</lease-time><remaining-time>3599</remaining-time></dhcp-lease></state></ipv4></routed-vlan></interface>"#
    ));
}

#[test]
fn no_lease_without_dhcp_client() {
    let vlan1 = InterfaceState {
        dhcp_lease: Some(DhcpLease::default()),
        ..up(None)
    };
    let xml = state_xml(
        &applied(),
        &BTreeMap::from([("vlan1".to_string(), vlan1)]),
        None,
    );
    assert!(xml.contains("<addresses></addresses><state><dhcp-client>false</dhcp-client></state>"));
    assert!(!xml.contains("dhcp-lease"));
}

#[test]
fn ports_have_no_ipv4_state() {
    let xml = state_xml(
        &applied(),
        &BTreeMap::from([("lan1".to_string(), up(None))]),
        None,
    );
    assert!(!xml.contains("routed-vlan"));
}
