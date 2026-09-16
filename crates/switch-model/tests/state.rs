use std::collections::{BTreeMap, BTreeSet};

use switch_model::{state_xml, DesiredState, InterfaceState, Port, Svi};

fn applied() -> DesiredState {
    DesiredState {
        ports: BTreeMap::from([(
            "lan1".to_string(),
            Port {
                enabled: true,
                access_vlan: 1,
            },
        )]),
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
    assert_eq!(
        state_xml(&applied(), &BTreeMap::new()),
        r#"<interfaces xmlns="http://openconfig.net/yang/interfaces"></interfaces>"#
    );
}
