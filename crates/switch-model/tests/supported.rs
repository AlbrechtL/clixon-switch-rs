//! Rejection of configuration the switch does not implement, on a real clixon
//! transaction tree: YANG defaults filled in, operational state merged in,
//! and clixon's own top-level modules next to the interfaces.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use switch_model::validate;

const CLIXON_TARGET: &str = include_str!("data/clixon-target.json");

fn ports() -> BTreeSet<String> {
    (1..=8).map(|i| format!("lan{i}")).collect()
}

fn target() -> Value {
    serde_json::from_str(CLIXON_TARGET).unwrap()
}

fn interface<'a>(tree: &'a mut Value, name: &str) -> &'a mut Value {
    tree["openconfig-interfaces:interfaces"]["interface"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|i| i["name"] == name)
        .unwrap()
}

/// Validates the tree after `change`, expecting exactly one error.
fn rejected(change: impl FnOnce(&mut Value)) -> String {
    let mut tree = target();
    change(&mut tree);
    let errors = validate(&tree.to_string(), &ports())
        .expect_err("must be rejected")
        .0;
    assert_eq!(errors.len(), 1, "{errors:?}");
    errors[0].to_string()
}

#[test]
fn clixon_tree_with_defaults_and_state() {
    let state = validate(CLIXON_TARGET, &ports()).unwrap();
    assert_eq!(state.ports.len(), 8);
    assert!(!state.ports["lan2"].enabled);
    assert_eq!(state.svis["vlan1"].vlan, 1);
}

#[test]
fn unsupported_leaf() {
    let error = rejected(|t| interface(t, "lan1")["config"]["mtu"] = json!(1400));
    assert_eq!(error, "interface lan1: config/mtu is not supported");
}

#[test]
fn unsupported_container() {
    let error = rejected(|t| {
        interface(t, "lan1")["subinterfaces"] = json!({"subinterface": [{"index": 0}]})
    });
    assert_eq!(error, "interface lan1: subinterfaces is not supported");
}

#[test]
fn default_only_leaves() {
    let error = rejected(|t| interface(t, "lan1")["config"]["loopback-mode"] = json!("FACILITY"));
    assert_eq!(
        error,
        "interface lan1: config/loopback-mode FACILITY is not supported, only NONE"
    );

    let error = rejected(|t| {
        interface(t, "lan1")["openconfig-if-ethernet:ethernet"]["config"]["auto-negotiate"] =
            json!(false)
    });
    assert!(
        error.contains("ethernet/config/auto-negotiate false"),
        "{error}"
    );
}

#[test]
fn trunk_port() {
    let mut tree = target();
    tree["clixon-switch:vlans"]["vlan"]
        .as_array_mut()
        .unwrap()
        .push(json!({"vlan-id": "10", "config": {"vlan-id": 10, "status": "ACTIVE"}}));
    interface(&mut tree, "lan1")["openconfig-if-ethernet:ethernet"]
        ["openconfig-vlan:switched-vlan"]["config"] =
        json!({"interface-mode": "TRUNK", "native-vlan": 1, "trunk-vlans": [10]});
    let state = validate(&tree.to_string(), &ports()).unwrap();
    assert_eq!(state.ports["lan1"].native_vlan, Some(1));
    assert!(state.ports["lan1"].tagged_vlans.contains(&10));
}

#[test]
fn vlan_database_and_mode_with_defaults_and_state() {
    let state = validate(CLIXON_TARGET, &ports()).unwrap();
    assert!(state.vlans[&1].active);
    assert_eq!(state.vlans[&1].name.as_deref(), Some("default"));

    let error = rejected(|t| t["clixon-switch:vlans"]["vlan"][0]["config"]["color"] = json!("red"));
    assert_eq!(error, "vlans/vlan/config/color is not supported");
}

#[test]
fn other_tpid() {
    let error =
        rejected(|t| interface(t, "lan1")["config"]["openconfig-vlan:tpid"] = json!("TPID_0X88A8"));
    assert!(
        error.contains("config/tpid TPID_0X88A8 is not supported, only TPID_0X8100"),
        "{error}"
    );
}

#[test]
fn secondary_address() {
    let error = rejected(|t| {
        interface(t, "vlan1")["openconfig-vlan:routed-vlan"]["openconfig-if-ip:ipv4"]["addresses"]
            ["address"][0]["config"]["type"] = json!("SECONDARY")
    });
    assert!(
        error.contains("addresses/address/config/type SECONDARY"),
        "{error}"
    );
}

#[test]
fn ipv6_addresses() {
    let error = rejected(|t| {
        interface(t, "vlan1")["openconfig-vlan:routed-vlan"]["openconfig-if-ip:ipv6"]["addresses"] = json!({"address": [{"ip": "fd00::1", "config": {"ip": "fd00::1", "prefix-length": 64}}]})
    });
    assert_eq!(
        error,
        "interface vlan1: routed-vlan/ipv6/addresses is not supported"
    );
}

#[test]
fn state_is_not_checked() {
    let mut tree = target();
    interface(&mut tree, "lan1")["state"] = json!({"anything": {"at": "all"}});
    tree["some-module:state-only"] = json!({"x": 1});
    assert!(validate(&tree.to_string(), &ports()).is_ok());
}

#[test]
fn unsupported_and_semantic_errors_together() {
    let mut tree = target();
    interface(&mut tree, "lan1")["config"]["mtu"] = json!(1400);
    interface(&mut tree, "lan3")["openconfig-if-ethernet:ethernet"]
        ["openconfig-vlan:switched-vlan"]["config"] =
        json!({"interface-mode": "ACCESS", "access-vlan": 99});
    let errors = validate(&tree.to_string(), &ports()).unwrap_err();
    assert_eq!(errors.0.len(), 2, "{errors}");
}

#[test]
fn dhcp_client_is_supported() {
    let mut tree = target();
    let vlan1 = interface(&mut tree, "vlan1");
    vlan1["openconfig-vlan:routed-vlan"]["openconfig-if-ip:ipv4"]["config"]["dhcp-client"] =
        json!(true);
    let state = validate(&tree.to_string(), &ports()).unwrap();
    assert!(state.svis["vlan1"].dhcp_client);
}
