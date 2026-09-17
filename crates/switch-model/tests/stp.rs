use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use switch_model::{
    desired_state, state_xml, validate, BridgeId, BridgeState, Config, DesiredState, EdgePort,
    Errors, PortState, Stp, StpProtocol, StpState, TreePort, TreeState,
};

const FACTORY_DEFAULT: &str = include_str!("data/factory-default.json");

fn ports() -> BTreeSet<String> {
    (1..=8).map(|i| format!("lan{i}")).collect()
}

/// The factory default with `stp` as `/stp`.
fn with_stp(stp: Value) -> String {
    let mut tree: Value = serde_json::from_str(FACTORY_DEFAULT).unwrap();
    tree["openconfig-spanning-tree:stp"] = stp;
    tree.to_string()
}

fn state(stp: Value) -> Result<DesiredState, Errors> {
    desired_state(
        &Config::from_json(&with_stp(stp)).expect("valid JSON"),
        &ports(),
    )
}

fn stp(stp: Value) -> Stp {
    state(stp)
        .expect("valid")
        .stp
        .expect("spanning tree enabled")
}

/// Expects exactly one validation error and returns it.
fn error(stp: Value) -> String {
    let errors = state(stp).expect_err("config must be rejected").0;
    assert_eq!(errors.len(), 1, "{errors:?}");
    errors[0].to_string()
}

fn enabled(protocol: &str) -> Value {
    json!({"enabled-protocol": [protocol]})
}

#[test]
fn off_by_default() {
    assert_eq!(state(json!({})).unwrap().stp, None);
    let no_protocol = json!({"global": {"config": {"enabled-protocol": []}},
                             "rstp": {"config": {"bridge-priority": 4096}}});
    assert_eq!(state(no_protocol).unwrap().stp, None);
}

#[test]
fn rstp_defaults() {
    let s = stp(json!({"global": {"config": enabled("openconfig-spanning-tree-types:RSTP")}}));
    assert_eq!(s.protocol, StpProtocol::Rstp);
    assert_eq!((s.hello_time, s.max_age, s.forward_delay), (2, 20, 15));
    assert_eq!((s.hold_count, s.max_hops), (6, 20));
    assert_eq!(s.cist.bridge_priority, 32768);
    assert_eq!(s.cist.ports.len(), 8);
    assert_eq!(s.cist.ports["lan1"], TreePort::default());
    assert!(s.mstis.is_empty());
    let lan1 = s.ports["lan1"];
    assert_eq!(lan1.edge, EdgePort::Auto);
    assert_eq!(lan1.point_to_point, None);
    assert!(!lan1.root_guard && !lan1.bpdu_guard && !lan1.bpdu_filter);
}

#[test]
fn stp_identity_uses_rstp_settings() {
    let s = stp(json!({
        "global": {"config": enabled("clixon-switch:STP")},
        "rstp": {"config": {"max-age": 30, "forwarding-delay": 20, "bridge-priority": 8192},
                 "interfaces": {"interface": [
                     {"name": "lan2", "config": {"name": "lan2", "cost": 4, "port-priority": 32}}]}},
        "mstp": {"config": {"clixon-switch:bridge-priority": 0}}
    }));
    assert_eq!(s.protocol, StpProtocol::Stp);
    assert_eq!((s.max_age, s.forward_delay), (30, 20));
    assert_eq!(s.cist.bridge_priority, 8192);
    assert_eq!(
        s.cist.ports["lan2"],
        TreePort {
            cost: Some(4),
            priority: 32
        }
    );
}

#[test]
fn mstp_region_cist_and_instances() {
    let s = stp(json!({
        "global": {"config": enabled("openconfig-spanning-tree-types:MSTP")},
        "rstp": {"config": {"bridge-priority": 4096}},
        "mstp": {
            "config": {"name": "region1", "revision": 3, "max-hop": 10,
                       "clixon-switch:bridge-priority": 12288},
            "clixon-switch:interfaces": {"interface": [
                {"name": "lan1", "config": {"name": "lan1", "cost": 100}}]},
            "mst-instances": {"mst-instance": [
                {"mst-id": 5, "config": {"mst-id": 5, "vlan": [10, "20..22"], "bridge-priority": 0},
                 "interfaces": {"interface": [
                     {"name": "lan8", "config": {"name": "lan8", "port-priority": 240}}]}},
                {"mst-id": 7, "config": {"mst-id": 7}}]}
        }
    }));
    assert_eq!(s.protocol, StpProtocol::Mstp);
    assert_eq!(s.region_name.as_deref(), Some("region1"));
    assert_eq!((s.region_revision, s.max_hops), (3, 10));
    assert_eq!(s.cist.bridge_priority, 12288);
    assert_eq!(s.cist.ports["lan1"].cost, Some(100));
    assert_eq!(s.mstis.len(), 2);
    let msti = &s.mstis[&5];
    assert_eq!(msti.vlans, BTreeSet::from([10, 20, 21, 22]));
    assert_eq!(msti.tree.bridge_priority, 0);
    assert_eq!(msti.tree.ports["lan8"].priority, 240);
    assert!(s.mstis[&7].vlans.is_empty());
}

#[test]
fn interface_features() {
    let s = stp(json!({
        "global": {"config": {"enabled-protocol": ["openconfig-spanning-tree-types:RSTP"],
                              "bpdu-guard": true}},
        "interfaces": {"interface": [
            {"name": "lan1", "config": {"name": "lan1",
                "edge-port": "openconfig-spanning-tree-types:EDGE_ENABLE",
                "link-type": "P2P", "guard": "ROOT", "bpdu-guard": false, "bpdu-filter": true}},
            {"name": "lan2", "config": {"name": "lan2",
                "edge-port": "openconfig-spanning-tree-types:EDGE_DISABLE", "link-type": "SHARED"}}]}
    }));
    let lan1 = s.ports["lan1"];
    assert_eq!(lan1.edge, EdgePort::Enable);
    assert_eq!(lan1.point_to_point, Some(true));
    assert!(lan1.root_guard && !lan1.bpdu_guard && lan1.bpdu_filter);
    let lan2 = s.ports["lan2"];
    assert_eq!(lan2.edge, EdgePort::Disable);
    assert_eq!(lan2.point_to_point, Some(false));
    assert!(lan2.bpdu_guard);
    assert!(s.ports["lan3"].bpdu_guard);
}

#[test]
fn rejected_configurations() {
    let rstp = |extra: Value| {
        let mut stp = json!({"global": {"config": enabled("RSTP")}});
        stp.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        stp
    };
    assert_eq!(
        error(json!({"global": {"config": {"enabled-protocol": ["RSTP", "MSTP"]}}})),
        "stp: enabled-protocol: only one protocol may be enabled"
    );
    assert_eq!(
        error(json!({"global": {"config": enabled("openconfig-spanning-tree-types:RAPID_PVST")}})),
        "stp: enabled-protocol RAPID_PVST is not supported, only RSTP, MSTP and STP"
    );
    assert_eq!(
        error(rstp(json!({"rstp": {"config": {"hello-time": 1}}}))),
        "stp: rstp: hello-time 1 is not supported, only 2"
    );
    assert_eq!(
        error(rstp(
            json!({"rstp": {"config": {"max-age": 40, "forwarding-delay": 15}}})
        )),
        "stp: rstp: max-age 40 is greater than 2 * (forwarding-delay 15 - 1)"
    );
    assert_eq!(
        error(rstp(json!({"rstp": {"config": {"bridge-priority": 1000}}}))),
        "stp: rstp: bridge-priority 1000 is not a multiple of 4096 in 0..61440"
    );
    assert_eq!(
        error(rstp(json!({"rstp": {"interfaces": {"interface": [
            {"name": "lan1", "config": {"name": "lan1", "port-priority": 100}}]}}}))),
        "stp: rstp: interface lan1: port-priority 100 is not a multiple of 16"
    );
    assert_eq!(
        error(rstp(json!({"rstp": {"interfaces": {"interface": [
            {"name": "vlan1", "config": {"name": "vlan1"}}]}}}))),
        "stp: rstp: interface vlan1 is not a configured switch port"
    );
    assert_eq!(
        error(rstp(json!({"interfaces": {"interface": [
            {"name": "lan1", "config": {"name": "lan1", "guard": "LOOP"}}]}}))),
        "stp: interfaces: interface lan1: guard LOOP is not supported, only ROOT and NONE"
    );
    assert_eq!(
        error(rstp(json!({"mstp": {"config": {"max-hop": 3}}}))),
        "stp: mstp: max-hop 3 is out of range 6..40"
    );
    assert_eq!(
        error(rstp(json!({"mstp": {"config": {"revision": 70000}}}))),
        "stp: mstp: revision 70000 is out of range 0..65535"
    );
    assert_eq!(
        error(rstp(json!({"mstp": {"mst-instances": {"mst-instance": [
            {"mst-id": 1, "config": {"mst-id": 1, "vlan": ["10..20"]}},
            {"mst-id": 2, "config": {"mst-id": 2, "vlan": [20]}}]}}}))),
        "stp: mstp mst-instance 2: VLAN 20 is already mapped to mst-instance 1"
    );
    assert_eq!(
        error(rstp(json!({"mstp": {"mst-instances": {"mst-instance": [
            {"mst-id": 1, "config": {"mst-id": 1, "vlan": ["20..10"]}}]}}}))),
        "stp: mstp mst-instance 1: vlan range 20..10: 20 is not below 10"
    );
}

#[test]
fn unsupported_nodes() {
    let rejected = |stp: Value| {
        let errors = validate(&with_stp(stp), &ports())
            .expect_err("must be rejected")
            .0;
        assert_eq!(errors.len(), 1, "{errors:?}");
        errors[0].to_string()
    };
    assert_eq!(
        rejected(json!({"global": {"config": {"loop-guard": true}}})),
        "stp/global/config/loop-guard true is not supported, only false"
    );
    assert_eq!(
        rejected(json!({"global": {"config": {"bpduguard-timeout-recovery": 30}}})),
        "stp/global/config/bpduguard-timeout-recovery is not supported"
    );
    assert_eq!(
        rejected(json!({"rapid-pvst": {"vlan": [{"vlan-id": 1}]}})),
        "stp/rapid-pvst is not supported"
    );
    // clixon's defaults and state are accepted.
    let defaults = json!({
        "global": {"config": {"bridge-assurance": false, "loop-guard": false},
                   "state": {"enabled-protocol": ["RSTP"]}},
        "rstp": {"config": {"hold-count": 6, "bridge-priority": 32768}},
        "mstp": {"config": {"hold-count": 6, "clixon-switch:bridge-priority": 32768}}
    });
    assert!(validate(&with_stp(defaults), &ports()).is_ok());
}

#[test]
fn state_data() {
    let mut applied = desired_state(
        &Config::from_json(&with_stp(json!({
            "global": {"config": enabled("MSTP")},
            "mstp": {"config": {"name": "r&d"},
                     "mst-instances": {"mst-instance": [
                         {"mst-id": 5, "config": {"mst-id": 5, "vlan": [10, 11, 12, 20]}}]}}
        })))
        .unwrap(),
        &ports(),
    )
    .unwrap();
    applied.ports.retain(|name, _| name == "lan1");
    let stp = applied.stp.as_mut().unwrap();
    stp.ports.retain(|name, _| name == "lan1");
    stp.cist.ports.retain(|name, _| name == "lan1");
    stp.mstis
        .get_mut(&5)
        .unwrap()
        .tree
        .ports
        .retain(|name, _| name == "lan1");

    let root = BridgeId {
        priority: 4096,
        address: "02:00:00:00:00:01".into(),
    };
    let reported = StpState {
        cist: TreeState {
            bridge: Some(BridgeState {
                bridge: None,
                root: Some(root.clone()),
                root_port: Some("lan1".into()),
                root_cost: Some(20000),
                topology_changes: Some(2),
            }),
            ports: BTreeMap::from([(
                "lan1".to_string(),
                PortState {
                    port_num: Some(1),
                    role: Some("ROOT"),
                    port_state: Some("FORWARDING"),
                    bpdu_received: Some(9),
                    ..PortState::default()
                },
            )]),
        },
        mstis: BTreeMap::new(),
    };
    let xml = state_xml(&applied, &BTreeMap::new(), Some(&reported));
    let stp_xml = &xml[xml.find("<stp ").expect("stp state")..];

    assert!(stp_xml.contains(
        r#"<enabled-protocol xmlns:sw="urn:github:albrechtl:clixon-switch" xmlns:oc-stp-types="http://openconfig.net/yang/spanning-tree/types">oc-stp-types:MSTP</enabled-protocol>"#
    ), "{stp_xml}");
    assert!(stp_xml
        .contains("<mstp><state><name>r&amp;d</name><revision>0</revision><max-hop>20</max-hop>"));
    assert!(stp_xml.contains(
        r#"<bridge-priority xmlns="urn:github:albrechtl:clixon-switch">32768</bridge-priority><designated-root-priority xmlns="urn:github:albrechtl:clixon-switch">4096</designated-root-priority><designated-root-address xmlns="urn:github:albrechtl:clixon-switch">02:00:00:00:00:01</designated-root-address><root-port xmlns="urn:github:albrechtl:clixon-switch">lan1</root-port>"#
    ), "{stp_xml}");
    assert!(stp_xml.contains("<mst-instance><mst-id>5</mst-id><state><mst-id>5</mst-id><vlan>10..12</vlan><vlan>20</vlan><bridge-priority>32768</bridge-priority></state>"));
    assert!(stp_xml.contains(
        r#"<interfaces xmlns="urn:github:albrechtl:clixon-switch"><interface><name>lan1</name><state><name>lan1</name><port-priority>128</port-priority><port-num>1</port-num><role xmlns:oc-stp-types="http://openconfig.net/yang/spanning-tree/types">oc-stp-types:ROOT</role>"#
    ), "{stp_xml}");
    assert!(stp_xml.contains("<counters><bpdu-received>9</bpdu-received></counters>"));
    assert!(stp_xml.ends_with(
        r#"<interfaces><interface><name>lan1</name><state><name>lan1</name><edge-port xmlns:oc-stp-types="http://openconfig.net/yang/spanning-tree/types">oc-stp-types:EDGE_AUTO</edge-port><guard>NONE</guard><bpdu-guard>false</bpdu-guard><bpdu-filter>false</bpdu-filter></state></interface></interfaces></stp>"#
    ), "{stp_xml}");

    // Spanning tree off: no /stp state at all.
    applied.stp = None;
    assert!(!state_xml(&applied, &BTreeMap::new(), None).contains("<stp"));
}
