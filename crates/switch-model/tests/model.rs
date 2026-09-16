use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use switch_model::{desired_state, Config, DesiredState, Errors, Ipv4Prefix, Port, Vlan, VlanMode};

const FACTORY_DEFAULT: &str = include_str!("data/factory-default.json");

fn ports() -> BTreeSet<String> {
    (1..=8).map(|i| format!("lan{i}")).collect()
}

fn state(json: &str) -> Result<DesiredState, Errors> {
    desired_state(&Config::from_json(json).expect("valid JSON"), &ports())
}

/// Expects exactly one validation error and returns its message.
fn error(json: &str) -> String {
    let errors = state(json).expect_err("config must be rejected").0;
    assert_eq!(errors.len(), 1, "{errors:?}");
    errors[0].message.clone()
}

/// The VLAN database of most tests: 1, 2, 10 "management", 20, 30.
const VLANS: &str = r#""clixon-switch:vlans": {"vlan": [
    {"vlan-id": 1, "config": {"vlan-id": 1, "name": "default"}},
    {"vlan-id": 2, "config": {"vlan-id": 2}},
    {"vlan-id": 10, "config": {"vlan-id": 10, "name": "management"}},
    {"vlan-id": 20, "config": {"vlan-id": 20, "status": "ACTIVE"}},
    {"vlan-id": 30, "config": {"vlan-id": 30}}]}"#;

/// Interfaces with the VLAN database [`VLANS`].
fn interfaces(interfaces: &[String]) -> String {
    format!(
        r#"{{{VLANS}, "openconfig-interfaces:interfaces": {{"interface": [{}]}}}}"#,
        interfaces.join(", ")
    )
}

fn one_interface(interface: &str) -> String {
    interfaces(&[interface.to_string()])
}

/// A trunk port; `native` and `trunk_vlans` are JSON or empty.
fn trunk_port(name: &str, native: &str, trunk_vlans: &str) -> String {
    let mut config = String::from(r#""interface-mode": "TRUNK""#);
    if !native.is_empty() {
        config += &format!(r#", "native-vlan": {native}"#);
    }
    if !trunk_vlans.is_empty() {
        config += &format!(r#", "trunk-vlans": {trunk_vlans}"#);
    }
    format!(
        r#"{{"name": "{name}",
            "config": {{"name": "{name}", "type": "iana-if-type:ethernetCsmacd"}},
            "openconfig-if-ethernet:ethernet": {{"openconfig-vlan:switched-vlan":
                {{"config": {{{config}}}}}}}}}"#
    )
}

/// A port without switched-vlan, as in port-based mode.
fn plain_port(name: &str) -> String {
    format!(
        r#"{{"name": "{name}", "config": {{"name": "{name}", "type": "iana-if-type:ethernetCsmacd"}}}}"#
    )
}

/// A port-based configuration: `groups` are JSON group entries.
fn port_based(groups: &str, interfaces: &[String]) -> String {
    format!(
        r#"{{"clixon-switch:switch": {{"config": {{"vlan-mode": "PORT_BASED"}}}},
            "clixon-switch:port-based-vlans": {{"group": [{groups}]}},
            "openconfig-interfaces:interfaces": {{"interface": [{}]}}}}"#,
        interfaces.join(", ")
    )
}

fn group(id: u16, name: &str, ports: &[&str]) -> String {
    let ports: Vec<String> = ports.iter().map(|p| format!("\"{p}\"")).collect();
    format!(
        r#"{{"id": {id}, "config": {{"id": {id}, "name": "{name}", "port": [{}]}}}}"#,
        ports.join(", ")
    )
}

fn tagged(vlans: &[u16]) -> BTreeSet<u16> {
    vlans.iter().copied().collect()
}

fn access_port(name: &str, vlan: &str) -> String {
    format!(
        r#"{{"name": "{name}",
            "config": {{"name": "{name}", "type": "iana-if-type:ethernetCsmacd"}},
            "openconfig-if-ethernet:ethernet": {{"openconfig-vlan:switched-vlan":
                {{"config": {{"interface-mode": "ACCESS", "access-vlan": {vlan}}}}}}}}}"#
    )
}

fn svi(name: &str, vlan: &str, ip: &str) -> String {
    format!(
        r#"{{"name": "{name}",
            "config": {{"name": "{name}", "type": "iana-if-type:l3ipvlan"}},
            "openconfig-vlan:routed-vlan": {{
                "config": {{"vlan": {vlan}}},
                "openconfig-if-ip:ipv4": {{"addresses": {{"address": [
                    {{"ip": "{ip}", "config": {{"ip": "{ip}", "prefix-length": 24}}}}]}}}}}}}}"#
    )
}

#[test]
fn factory_default() {
    let state = state(FACTORY_DEFAULT).unwrap();

    assert_eq!(state.ports.len(), 8);
    for (name, port) in &state.ports {
        assert!(name.starts_with("lan"));
        assert_eq!(*port, Port::access(1));
    }
    assert_eq!(state.mode, VlanMode::Dot1q);
    assert_eq!(
        state.vlans,
        BTreeMap::from([(
            1,
            Vlan {
                name: Some("default".into()),
                active: true
            }
        )])
    );
    let vlan1 = &state.svis["vlan1"];
    assert_eq!(state.svis.len(), 1);
    assert!(vlan1.enabled);
    assert_eq!(vlan1.vlan, 1);
    assert_eq!(
        vlan1.addresses,
        BTreeSet::from([Ipv4Prefix {
            addr: Ipv4Addr::new(192, 168, 1, 1),
            prefix_len: 24
        }])
    );
}

#[test]
fn empty_datastore() {
    assert_eq!(state("").unwrap(), DesiredState::default());
    assert_eq!(state("{}").unwrap(), DesiredState::default());
}

#[test]
fn lenient_scalars_and_prefixed_identities() {
    // Numbers as strings, identity with the YANG prefix instead of the module
    // name, and enabled left out (YANG default true).
    let json = one_interface(
        r#"{"name": "lan3",
            "config": {"name": "lan3", "type": "ianaift:ethernetCsmacd"},
            "openconfig-if-ethernet:ethernet": {"openconfig-vlan:switched-vlan":
                {"config": {"access-vlan": "10"}}}}"#,
    );
    let state = state(&json).unwrap();
    assert_eq!(state.ports["lan3"], Port::access(10));
}

#[test]
fn unknown_leaves_are_ignored() {
    let json = one_interface(
        r#"{"name": "lan1",
            "config": {"name": "lan1", "type": "iana-if-type:ethernetCsmacd", "mtu": 1500},
            "hold-time": {"config": {"up": 0}},
            "openconfig-if-ethernet:ethernet": {
                "config": {"auto-negotiate": true},
                "openconfig-vlan:switched-vlan":
                    {"config": {"interface-mode": "ACCESS", "access-vlan": 1}}}}"#,
    );
    assert!(state(&json).is_ok());
}

#[test]
fn disabled_port() {
    let json = one_interface(
        r#"{"name": "lan2",
            "config": {"name": "lan2", "type": "iana-if-type:ethernetCsmacd", "enabled": false},
            "openconfig-if-ethernet:ethernet": {"openconfig-vlan:switched-vlan":
                {"config": {"interface-mode": "ACCESS", "access-vlan": 1}}}}"#,
    );
    assert!(!state(&json).unwrap().ports["lan2"].enabled);
}

#[test]
fn port_that_does_not_exist() {
    let message = error(&one_interface(&access_port("lan9", "1")));
    assert!(message.contains("not a port of this switch"), "{message}");
}

#[test]
fn port_without_vlan() {
    let json = one_interface(
        r#"{"name": "lan1", "config": {"name": "lan1", "type": "iana-if-type:ethernetCsmacd"}}"#,
    );
    assert!(error(&json).contains("switched-vlan"));
}

#[test]
fn unsupported_interface_type() {
    let json = one_interface(
        r#"{"name": "lo0", "config": {"name": "lo0", "type": "iana-if-type:softwareLoopback"}}"#,
    );
    assert!(error(&json).contains("not supported"));
}

#[test]
fn svi_on_vlan_name() {
    let state = state(&one_interface(&svi("mgmt", "\"management\"", "10.0.0.1"))).unwrap();
    assert_eq!(state.svis["mgmt"].vlan, 10);

    let message = error(&one_interface(&svi("mgmt", "\"guests\"", "10.0.0.1")));
    assert!(message.contains("no VLAN has this name"), "{message}");
}

#[test]
fn vlan_id_out_of_range() {
    assert!(error(&one_interface(&svi("vlan4095", "4095", "10.0.0.1"))).contains("out of range"));
}

#[test]
fn svi_names() {
    for bad in ["br-lan", "lan1", "a-name-longer-than-15", "vlan/1"] {
        assert!(
            state(&one_interface(&svi(bad, "1", "10.0.0.1"))).is_err(),
            "{bad} must be rejected"
        );
    }
}

#[test]
fn two_svis_on_one_vlan() {
    let json = interfaces(&[svi("vlan1", "1", "10.0.0.1"), svi("mgmt", "1", "10.0.1.1")]);
    assert!(error(&json).contains("already has a routed interface"));
}

#[test]
fn same_address_on_two_svis() {
    let json = interfaces(&[svi("vlan1", "1", "10.0.0.1"), svi("vlan2", "2", "10.0.0.1")]);
    assert!(error(&json).contains("already configured"));
}

#[test]
fn all_errors_are_reported() {
    let json = interfaces(&[access_port("lan9", "1"), svi("br-lan", "1", "10.0.0.1")]);
    let errors = state(&json).unwrap_err();
    assert_eq!(errors.0.len(), 2, "{errors}");
}

// ---------------------------------------------------------------------------
// 802.1Q: VLAN database and trunks
// ---------------------------------------------------------------------------

#[test]
fn undeclared_vlan() {
    for json in [
        one_interface(&access_port("lan1", "99")),
        one_interface(&trunk_port("lan1", "99", "")),
        one_interface(&trunk_port("lan1", "", "[10, 99]")),
        one_interface(&svi("vlan99", "99", "10.0.0.1")),
    ] {
        let message = error(&json);
        assert!(
            message.contains("VLAN 99 is not declared in vlans"),
            "{message}"
        );
    }
}

#[test]
fn no_vlan_database() {
    let json = r#"{"openconfig-interfaces:interfaces": {"interface": [
        {"name": "lan1",
         "config": {"name": "lan1", "type": "iana-if-type:ethernetCsmacd"},
         "openconfig-if-ethernet:ethernet": {"openconfig-vlan:switched-vlan":
             {"config": {"interface-mode": "ACCESS", "access-vlan": 1}}}}]}}"#;
    assert!(error(json).contains("VLAN 1 is not declared"));
}

#[test]
fn trunk_with_vlan_list() {
    let state = state(&one_interface(&trunk_port("lan1", "1", "[10, 20]"))).unwrap();
    let port = &state.ports["lan1"];
    assert_eq!(port.native_vlan, Some(1));
    assert_eq!(port.tagged_vlans, tagged(&[10, 20]));
}

#[test]
fn trunk_with_range() {
    // A range covers the declared VLANs in it only.
    let state = state(&one_interface(&trunk_port("lan1", "", r#"["2..25", 30]"#))).unwrap();
    let port = &state.ports["lan1"];
    assert_eq!(port.native_vlan, None);
    assert_eq!(port.tagged_vlans, tagged(&[2, 10, 20, 30]));
}

#[test]
fn trunk_without_list_carries_all_vlans() {
    let state = state(&one_interface(&trunk_port("lan1", "1", ""))).unwrap();
    let port = &state.ports["lan1"];
    // The native VLAN is untagged only.
    assert_eq!(port.native_vlan, Some(1));
    assert_eq!(port.tagged_vlans, tagged(&[2, 10, 20, 30]));
    assert!(port.carries(1) && port.carries(30) && !port.carries(3));
}

#[test]
fn native_vlan_in_trunk_list() {
    let state = state(&one_interface(&trunk_port("lan1", "10", "[1, 10]"))).unwrap();
    assert_eq!(state.ports["lan1"].tagged_vlans, tagged(&[1]));
}

#[test]
fn bad_trunk_ranges() {
    for (entry, expected) in [
        (r#"["20..10"]"#, "is not below"),
        (r#"["1..4095"]"#, "out of range"),
        (r#"["ten"]"#, "neither a VLAN id nor a range"),
    ] {
        let message = error(&one_interface(&trunk_port("lan1", "", entry)));
        assert!(message.contains(expected), "{entry}: {message}");
    }
}

#[test]
fn leaves_of_the_other_interface_mode() {
    let json = one_interface(
        r#"{"name": "lan1",
            "config": {"name": "lan1", "type": "iana-if-type:ethernetCsmacd"},
            "openconfig-if-ethernet:ethernet": {"openconfig-vlan:switched-vlan":
                {"config": {"interface-mode": "ACCESS", "access-vlan": 1, "trunk-vlans": [10]}}}}"#,
    );
    assert!(error(&json).contains("require interface-mode TRUNK"));

    let json = one_interface(
        r#"{"name": "lan1",
            "config": {"name": "lan1", "type": "iana-if-type:ethernetCsmacd"},
            "openconfig-if-ethernet:ethernet": {"openconfig-vlan:switched-vlan":
                {"config": {"interface-mode": "TRUNK", "access-vlan": 1}}}}"#,
    );
    assert!(error(&json).contains("requires interface-mode ACCESS"));
}

#[test]
fn suspended_vlan() {
    let json = format!(
        r#"{{"clixon-switch:vlans": {{"vlan": [
                {{"vlan-id": 1, "config": {{"vlan-id": 1}}}},
                {{"vlan-id": 20, "config": {{"vlan-id": 20, "status": "SUSPENDED"}}}}]}},
            "openconfig-interfaces:interfaces": {{"interface": [{}, {}, {}]}}}}"#,
        access_port("lan1", "20"),
        trunk_port("lan2", "20", ""),
        svi("vlan20", "20", "10.0.0.1"),
    );
    let state = state(&json).unwrap();
    assert!(!state.vlans[&20].active);
    assert!(!state.vlan_active(20));
    assert_eq!(state.ports["lan1"].native_vlan, None);
    assert_eq!(state.ports["lan2"].native_vlan, None);
    assert_eq!(state.ports["lan2"].tagged_vlans, tagged(&[1]));
    // The SVI stays; the planner gives the bridge no entry for it.
    assert_eq!(state.svis["vlan20"].vlan, 20);
}

#[test]
fn ambiguous_vlan_name() {
    let json = format!(
        r#"{{"clixon-switch:vlans": {{"vlan": [
                {{"vlan-id": 1, "config": {{"vlan-id": 1, "name": "office"}}}},
                {{"vlan-id": 2, "config": {{"vlan-id": 2, "name": "office"}}}}]}},
            "openconfig-interfaces:interfaces": {{"interface": [{}]}}}}"#,
        svi("office", "\"office\"", "10.0.0.1"),
    );
    assert!(error(&json).contains("ambiguous (VLANs 1, 2)"));
}

#[test]
fn port_based_groups_in_dot1q_mode() {
    let json = format!(
        r#"{{{VLANS}, "clixon-switch:port-based-vlans": {{"group": [{}]}}}}"#,
        group(1, "a", &[])
    );
    assert!(error(&json).contains("requires switch vlan-mode PORT_BASED"));
}

// ---------------------------------------------------------------------------
// Port-based VLAN groups
// ---------------------------------------------------------------------------

#[test]
fn port_based_groups() {
    let json = port_based(
        &[
            group(1, "office", &["lan1", "lan2"]),
            group(2, "lab", &["lan3"]),
            group(3, "empty", &[]),
        ]
        .join(", "),
        &[
            plain_port("lan1"),
            plain_port("lan2"),
            plain_port("lan3"),
            svi("vlan1", "\"office\"", "192.168.1.1"),
        ],
    );
    let state = state(&json).unwrap();
    assert_eq!(state.mode, VlanMode::PortBased);
    assert_eq!(state.vlans.keys().copied().collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(state.ports["lan1"], Port::access(1));
    assert_eq!(state.ports["lan2"], Port::access(1));
    assert_eq!(state.ports["lan3"], Port::access(2));
    assert_eq!(state.svis["vlan1"].vlan, 1);
}

#[test]
fn port_in_two_groups() {
    let json = port_based(
        &[group(1, "a", &["lan1"]), group(2, "b", &["lan1"])].join(", "),
        &[plain_port("lan1")],
    );
    let message = error(&json);
    assert!(
        message.contains("lan1 is already a member of group 1"),
        "{message}"
    );
}

#[test]
fn port_in_no_group() {
    let json = port_based(
        &group(1, "a", &["lan1"]),
        &[plain_port("lan1"), plain_port("lan2")],
    );
    assert!(error(&json).contains("not a member of any port-based-vlans group"));
}

#[test]
fn group_member_that_is_not_a_switch_port() {
    let json = port_based(
        &group(1, "a", &["vlan1"]),
        &[svi("vlan1", "1", "192.168.1.1")],
    );
    assert!(error(&json).contains("vlan1 is not a configured switch port"));
}

#[test]
fn dot1q_configuration_in_port_based_mode() {
    let json = port_based(&group(1, "a", &["lan1"]), &[access_port("lan1", "1")]);
    assert!(error(&json).contains("switched-vlan requires switch vlan-mode DOT1Q"));

    let json = format!(
        r#"{{"clixon-switch:switch": {{"config": {{"vlan-mode": "PORT_BASED"}}}}, {VLANS}}}"#
    );
    assert!(error(&json).contains("vlans requires switch vlan-mode DOT1Q"));
}

#[test]
fn svi_on_missing_group() {
    let json = port_based(&group(1, "a", &[]), &[svi("vlan5", "5", "192.168.1.1")]);
    assert!(error(&json).contains("port-based-vlans group 5 does not exist"));
}
