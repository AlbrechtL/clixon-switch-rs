use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use switch_model::{desired_state, Config, DesiredState, Errors, Ipv4Prefix, Port};

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

fn one_interface(interface: &str) -> String {
    format!(r#"{{"openconfig-interfaces:interfaces": {{"interface": [{interface}]}}}}"#)
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
        assert_eq!(
            *port,
            Port {
                enabled: true,
                access_vlan: 1
            }
        );
    }

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
    assert_eq!(
        state.ports["lan3"],
        Port {
            enabled: true,
            access_vlan: 10
        }
    );
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
fn trunk_is_not_supported_yet() {
    let json = one_interface(
        r#"{"name": "lan1",
            "config": {"name": "lan1", "type": "iana-if-type:ethernetCsmacd"},
            "openconfig-if-ethernet:ethernet": {"openconfig-vlan:switched-vlan":
                {"config": {"interface-mode": "TRUNK", "trunk-vlans": [10, 20]}}}}"#,
    );
    assert!(error(&json).contains("TRUNK"));
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
fn vlan_name_instead_of_id() {
    let message = error(&one_interface(&svi("mgmt", "\"management\"", "10.0.0.1")));
    assert!(message.contains("VLAN names"), "{message}");
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
    let json = format!(
        r#"{{"openconfig-interfaces:interfaces": {{"interface": [{}, {}]}}}}"#,
        svi("vlan1", "1", "10.0.0.1"),
        svi("mgmt", "1", "10.0.1.1")
    );
    assert!(error(&json).contains("already has a routed interface"));
}

#[test]
fn same_address_on_two_svis() {
    let json = format!(
        r#"{{"openconfig-interfaces:interfaces": {{"interface": [{}, {}]}}}}"#,
        svi("vlan1", "1", "10.0.0.1"),
        svi("vlan2", "2", "10.0.0.1")
    );
    assert!(error(&json).contains("already configured"));
}

#[test]
fn all_errors_are_reported() {
    let json = format!(
        r#"{{"openconfig-interfaces:interfaces": {{"interface": [{}, {}]}}}}"#,
        access_port("lan9", "1"),
        svi("br-lan", "1", "10.0.0.1")
    );
    let errors = state(&json).unwrap_err();
    assert_eq!(errors.0.len(), 2, "{errors}");
}
