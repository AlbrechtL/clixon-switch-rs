//! BRIDGE-MIB, Q-BRIDGE-MIB and RSTP-MIB state data.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use switch_model::{
    bridge_mib_xml, port_numbers, validate, BridgeId, BridgeInfo, BridgeState, DesiredState,
    FdbEntry, FdbStatus, MibModules, PortState, PortVlans, StpState, TreeState,
};

const FACTORY_DEFAULT: &str = include_str!("data/factory-default.json");

const ALL: MibModules = MibModules {
    bridge: true,
    q_bridge: true,
    rstp: true,
};

fn ports() -> BTreeSet<String> {
    (1..=8).map(|i| format!("lan{i}")).collect()
}

/// The factory default with VLAN 20 "office" on lan3, and `change`.
fn applied(change: impl FnOnce(&mut Value)) -> DesiredState {
    let mut tree: Value = serde_json::from_str(FACTORY_DEFAULT).unwrap();
    tree["clixon-switch:vlans"]["vlan"]
        .as_array_mut()
        .unwrap()
        .push(json!({"vlan-id": 20, "config": {"vlan-id": 20, "name": "office"}}));
    tree["openconfig-interfaces:interfaces"]["interface"][2]["openconfig-if-ethernet:ethernet"]
        ["openconfig-vlan:switched-vlan"]["config"]["access-vlan"] = json!(20);
    change(&mut tree);
    validate(&tree.to_string(), &ports()).unwrap()
}

/// The kernel's bridge VLANs for `applied`: access ports.
fn port_vlans(applied: &DesiredState) -> BTreeMap<String, PortVlans> {
    applied
        .ports
        .iter()
        .map(|(name, port)| {
            let vid = port.native_vlan.unwrap();
            (
                name.clone(),
                PortVlans {
                    pvid: Some(vid),
                    vlans: BTreeMap::from([(vid, true)]),
                },
            )
        })
        .collect()
}

fn ifindex() -> BTreeMap<String, u32> {
    ports().into_iter().zip(10..).collect()
}

fn xml(applied: &DesiredState, fdb: Option<&[FdbEntry]>, stp: Option<&StpState>) -> String {
    let vlans = port_vlans(applied);
    let ifindex = ifindex();
    let info = BridgeInfo {
        applied,
        bridge_mac: Some([0x02, 0, 0, 0, 0, 0x01]),
        ifindex: &ifindex,
        port_vlans: &vlans,
        fdb,
        stp,
    };
    bridge_mib_xml(&info, ALL)
}

#[test]
fn modules_of_a_request() {
    let modules = |xpath| MibModules::from_xpath(xpath);
    assert!(modules(Some("q-bridge:Q-BRIDGE-MIB/q-bridge:dot1qTpFdbTable")).q_bridge);
    assert!(modules(Some("/bridge-mib:BRIDGE-MIB")).bridge);
    assert!(!modules(Some("/bridge-mib:BRIDGE-MIB")).q_bridge);
    assert!(
        modules(Some(
            "rstp-mib:RSTP-MIB/rstp-mib:dot1dStp/rstp-mib:dot1dStpVersion"
        ))
        .rstp
    );
    assert!(!modules(Some("/")).any());
    assert!(!modules(None).any());
    assert!(!modules(Some("/oc-if:interfaces")).any());
}

#[test]
fn port_numbering() {
    let names = ["lan10", "lan2", "lan1", "cpu"].map(String::from);
    let numbers = port_numbers(&names);
    assert_eq!(numbers["lan1"], 1);
    assert_eq!(numbers["lan2"], 2);
    assert_eq!(numbers["lan10"], 3);
    assert_eq!(numbers["cpu"], 4);
}

#[test]
fn bridge_and_vlans() {
    let applied = applied(|_| {});
    let xml = xml(&applied, None, None);
    assert!(xml.starts_with(
        r#"<BRIDGE-MIB xmlns="urn:ietf:params:xml:ns:yang:smiv2:BRIDGE-MIB"><dot1dBase><dot1dBaseBridgeAddress>02:00:00:00:00:01</dot1dBaseBridgeAddress><dot1dBaseNumPorts>8</dot1dBaseNumPorts><dot1dBaseType>transparent-only</dot1dBaseType></dot1dBase>"#
    ), "{xml}");
    // lan3: port 3, ifindex 12, PVID 20.
    assert!(xml.contains(
        r#"<dot1dBasePortEntry><dot1dBasePort>3</dot1dBasePort><dot1dBasePortIfIndex>12</dot1dBasePortIfIndex><dot1dBasePortCircuit>0.0</dot1dBasePortCircuit><dot1qPvid xmlns="urn:ietf:params:xml:ns:yang:smiv2:Q-BRIDGE-MIB">20</dot1qPvid><dot1qPortAcceptableFrameTypes xmlns="urn:ietf:params:xml:ns:yang:smiv2:Q-BRIDGE-MIB">admitAll</dot1qPortAcceptableFrameTypes>"#
    ), "{xml}");
    // Spanning tree off: no dot1dStp, no RSTP-MIB, no FDB tables unless read.
    assert!(!xml.contains("dot1dStp"));
    assert!(!xml.contains("RSTP-MIB"));
    assert!(!xml.contains("TpFdb"));
    assert!(xml.contains("<dot1qNumVlans>2</dot1qNumVlans>"));
    // VLAN 1: ports 1, 2, 4..8 (0xdf), VLAN 20: port 3 (0x20).
    assert!(xml.contains(
        "<dot1qVlanCurrentEntry><dot1qVlanTimeMark>0</dot1qVlanTimeMark><dot1qVlanIndex>1</dot1qVlanIndex><dot1qVlanFdbId>1</dot1qVlanFdbId><dot1qVlanCurrentEgressPorts>3w==</dot1qVlanCurrentEgressPorts><dot1qVlanCurrentUntaggedPorts>3w==</dot1qVlanCurrentUntaggedPorts>"
    ), "{xml}");
    assert!(xml.contains(
        "<dot1qVlanStaticEntry><dot1qVlanIndex>20</dot1qVlanIndex><dot1qVlanStaticName>office</dot1qVlanStaticName><dot1qVlanStaticEgressPorts>IA==</dot1qVlanStaticEgressPorts><dot1qVlanForbiddenEgressPorts>AA==</dot1qVlanForbiddenEgressPorts><dot1qVlanStaticUntaggedPorts>IA==</dot1qVlanStaticUntaggedPorts><dot1qVlanStaticRowStatus>active</dot1qVlanStaticRowStatus></dot1qVlanStaticEntry>"
    ), "{xml}");
}

#[test]
fn trunk_port_without_native_vlan() {
    let applied = applied(|t| {
        t["openconfig-interfaces:interfaces"]["interface"][7]["openconfig-if-ethernet:ethernet"]
            ["openconfig-vlan:switched-vlan"]["config"] =
            json!({"interface-mode": "TRUNK", "trunk-vlans": [20]});
    });
    let mut vlans = port_vlans_for_trunk(&applied);
    vlans.get_mut("lan8").unwrap().pvid = None;
    let ifindex = ifindex();
    let info = BridgeInfo {
        applied: &applied,
        bridge_mac: None,
        ifindex: &ifindex,
        port_vlans: &vlans,
        fdb: None,
        stp: None,
    };
    let xml = bridge_mib_xml(
        &info,
        MibModules {
            bridge: true,
            ..MibModules::default()
        },
    );
    assert!(xml.contains("<dot1dBasePortEntry><dot1dBasePort>8</dot1dBasePort><dot1dBasePortIfIndex>17</dot1dBasePortIfIndex><dot1dBasePortCircuit>0.0</dot1dBasePortCircuit><dot1qPortAcceptableFrameTypes"), "{xml}");
    assert!(xml.contains("admitOnlyVlanTagged"));
    assert!(
        !xml.contains("Q-BRIDGE-MIB xmlns"),
        "only BRIDGE-MIB asked for"
    );
}

fn port_vlans_for_trunk(applied: &DesiredState) -> BTreeMap<String, PortVlans> {
    let mut vlans = BTreeMap::new();
    for (name, port) in &applied.ports {
        let mut entry = PortVlans {
            pvid: port.native_vlan,
            vlans: port.tagged_vlans.iter().map(|v| (*v, false)).collect(),
        };
        if let Some(native) = port.native_vlan {
            entry.vlans.insert(native, true);
        }
        vlans.insert(name.clone(), entry);
    }
    vlans
}

#[test]
fn forwarding_database() {
    let applied = applied(|_| {});
    let fdb = [
        FdbEntry {
            mac: [0xaa, 0xbb, 0xcc, 0, 0, 0x01],
            vlan: Some(20),
            port: "lan3".into(),
            status: FdbStatus::Learned,
        },
        FdbEntry {
            mac: [0xaa, 0xbb, 0xcc, 0, 0, 0x01],
            vlan: Some(1),
            port: "lan1".into(),
            status: FdbStatus::Learned,
        },
        FdbEntry {
            mac: [0x02, 0, 0, 0, 0, 0x01],
            vlan: None,
            port: "br-lan".into(),
            status: FdbStatus::Own,
        },
    ];
    let xml = xml(&applied, Some(&fdb), None);
    // BRIDGE-MIB: one entry per address.
    assert!(xml.contains("<dot1dTpFdbTable><dot1dTpFdbEntry><dot1dTpFdbAddress>aa:bb:cc:00:00:01</dot1dTpFdbAddress><dot1dTpFdbPort>3</dot1dTpFdbPort><dot1dTpFdbStatus>learned</dot1dTpFdbStatus></dot1dTpFdbEntry><dot1dTpFdbEntry><dot1dTpFdbAddress>02:00:00:00:00:01</dot1dTpFdbAddress><dot1dTpFdbPort>0</dot1dTpFdbPort><dot1dTpFdbStatus>self</dot1dTpFdbStatus></dot1dTpFdbEntry></dot1dTpFdbTable>"), "{xml}");
    // Q-BRIDGE-MIB: per VLAN, without entries that have none.
    assert!(xml.contains("<dot1qFdbTable><dot1qFdbEntry><dot1qFdbId>1</dot1qFdbId><dot1qFdbDynamicCount>1</dot1qFdbDynamicCount></dot1qFdbEntry><dot1qFdbEntry><dot1qFdbId>20</dot1qFdbId><dot1qFdbDynamicCount>1</dot1qFdbDynamicCount></dot1qFdbEntry></dot1qFdbTable>"), "{xml}");
    assert!(xml.contains("<dot1qTpFdbEntry><dot1qFdbId>1</dot1qFdbId><dot1qTpFdbAddress>aa:bb:cc:00:00:01</dot1qTpFdbAddress><dot1qTpFdbPort>1</dot1qTpFdbPort><dot1qTpFdbStatus>learned</dot1qTpFdbStatus></dot1qTpFdbEntry>"), "{xml}");
    assert_eq!(xml.matches("<dot1qTpFdbEntry>").count(), 2);
}

#[test]
fn spanning_tree() {
    let applied = applied(|t| {
        t["openconfig-spanning-tree:stp"] = json!({
            "global": {"config": {"enabled-protocol": ["openconfig-spanning-tree-types:RSTP"]}},
            "rstp": {"config": {"bridge-priority": 4096, "hold-count": 4},
                     "interfaces": {"interface": [{"name": "lan2", "config": {"name": "lan2", "cost": 100000}}]}},
            "interfaces": {"interface": [{"name": "lan2", "config": {"name": "lan2",
                "edge-port": "openconfig-spanning-tree-types:EDGE_ENABLE", "link-type": "P2P"}}]}
        });
    });
    let root = BridgeId {
        priority: 4096,
        address: "02:00:00:00:00:09".into(),
    };
    let state = StpState {
        cist: TreeState {
            bridge: Some(BridgeState {
                root: Some(root.clone()),
                root_port: Some("lan1".into()),
                root_cost: Some(20000),
                topology_changes: Some(3),
                time_since_topology_change: Some(12),
                ..BridgeState::default()
            }),
            ports: BTreeMap::from([
                (
                    "lan1".to_string(),
                    PortState {
                        port_state: Some("FORWARDING"),
                        designated_root: Some(root.clone()),
                        designated_bridge: Some(root.clone()),
                        designated_port_priority: Some(128),
                        designated_port_num: Some(5),
                        forward_transitions: Some(1),
                        path_cost: Some(20000),
                        oper_edge: Some(false),
                        oper_point_to_point: Some(true),
                        ..PortState::default()
                    },
                ),
                (
                    "lan2".to_string(),
                    PortState {
                        port_state: Some("BLOCKING"),
                        path_cost: Some(100000),
                        ..PortState::default()
                    },
                ),
                (
                    "lan4".to_string(),
                    PortState {
                        port_state: Some("DISABLED"),
                        ..PortState::default()
                    },
                ),
            ]),
        },
        mstis: BTreeMap::new(),
    };
    let xml = xml(&applied, None, Some(&state));
    // BridgeId 4096 and 02:00:00:00:00:09 is 10 00 02 00 00 00 00 09.
    assert!(xml.contains("<dot1dStp><dot1dStpProtocolSpecification>ieee8021d</dot1dStpProtocolSpecification><dot1dStpPriority>4096</dot1dStpPriority><dot1dStpTimeSinceTopologyChange>1200</dot1dStpTimeSinceTopologyChange><dot1dStpTopChanges>3</dot1dStpTopChanges><dot1dStpDesignatedRoot>EAACAAAAAAk=</dot1dStpDesignatedRoot><dot1dStpRootCost>20000</dot1dStpRootCost><dot1dStpRootPort>1</dot1dStpRootPort><dot1dStpMaxAge>2000</dot1dStpMaxAge>"), "{xml}");
    // lan1: designated port 8.005 is 0x8005, gA==.
    assert!(xml.contains("<dot1dStpPortEntry><dot1dStpPort>1</dot1dStpPort><dot1dStpPortPriority>128</dot1dStpPortPriority><dot1dStpPortState>forwarding</dot1dStpPortState><dot1dStpPortEnable>enabled</dot1dStpPortEnable><dot1dStpPortPathCost>20000</dot1dStpPortPathCost><dot1dStpPortDesignatedRoot>EAACAAAAAAk=</dot1dStpPortDesignatedRoot><dot1dStpPortDesignatedBridge>EAACAAAAAAk=</dot1dStpPortDesignatedBridge><dot1dStpPortDesignatedPort>gAU=</dot1dStpPortDesignatedPort><dot1dStpPortForwardTransitions>1</dot1dStpPortForwardTransitions><dot1dStpPortPathCost32>20000</dot1dStpPortPathCost32>"), "{xml}");
    assert!(xml.contains(r#"<dot1dStpPortOperEdgePort xmlns="urn:ietf:params:xml:ns:yang:smiv2:RSTP-MIB">false</dot1dStpPortOperEdgePort><dot1dStpPortAdminPointToPoint xmlns="urn:ietf:params:xml:ns:yang:smiv2:RSTP-MIB">auto</dot1dStpPortAdminPointToPoint><dot1dStpPortOperPointToPoint xmlns="urn:ietf:params:xml:ns:yang:smiv2:RSTP-MIB">true</dot1dStpPortOperPointToPoint>"#), "{xml}");
    // lan2: a 32-bit cost is 65535 in the 16-bit column; edge and P2P forced.
    assert!(xml.contains("<dot1dStpPortState>blocking</dot1dStpPortState><dot1dStpPortEnable>enabled</dot1dStpPortEnable><dot1dStpPortPathCost>65535</dot1dStpPortPathCost><dot1dStpPortPathCost32>100000</dot1dStpPortPathCost32>"), "{xml}");
    assert!(xml.contains(r#"<dot1dStpPortAdminEdgePort xmlns="urn:ietf:params:xml:ns:yang:smiv2:RSTP-MIB">true</dot1dStpPortAdminEdgePort><dot1dStpPortAdminPointToPoint xmlns="urn:ietf:params:xml:ns:yang:smiv2:RSTP-MIB">forceTrue</dot1dStpPortAdminPointToPoint><dot1dStpPortAdminPathCost xmlns="urn:ietf:params:xml:ns:yang:smiv2:RSTP-MIB">100000</dot1dStpPortAdminPathCost>"#), "{xml}");
    assert!(xml.contains("<dot1dStpPort>4</dot1dStpPort><dot1dStpPortPriority>128</dot1dStpPortPriority><dot1dStpPortState>disabled</dot1dStpPortState><dot1dStpPortEnable>disabled</dot1dStpPortEnable>"), "{xml}");
    assert!(xml.contains("<RSTP-MIB xmlns=\"urn:ietf:params:xml:ns:yang:smiv2:RSTP-MIB\"><dot1dStp><dot1dStpVersion>rstp</dot1dStpVersion><dot1dStpTxHoldCount>4</dot1dStpTxHoldCount></dot1dStp></RSTP-MIB>"), "{xml}");
}
