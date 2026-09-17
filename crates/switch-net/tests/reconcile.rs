use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

use switch_model::{DesiredState, Ipv4Prefix, Port, Svi, Vlan, VlanMode};
use switch_net::fake::FakeNet;
use switch_net::{
    plan, reconcile, BridgeStp, Link, LinkKind, NetBackend, Op, VlanFlags, BRIDGE_NAME,
};

const ACCESS: VlanFlags = VlanFlags {
    pvid: true,
    untagged: true,
};
const TAGGED: VlanFlags = VlanFlags {
    pvid: false,
    untagged: false,
};

fn prefix(a: u8, b: u8, c: u8, d: u8, len: u8) -> Ipv4Prefix {
    Ipv4Prefix {
        addr: Ipv4Addr::new(a, b, c, d),
        prefix_len: len,
    }
}

fn factory_default() -> DesiredState {
    DesiredState {
        mode: VlanMode::Dot1q,
        vlans: BTreeMap::from([(1, active())]),
        ports: (1..=8)
            .map(|i| (format!("lan{i}"), Port::access(1)))
            .collect(),
        svis: BTreeMap::from([(
            "vlan1".to_string(),
            Svi {
                enabled: true,
                vlan: 1,
                addresses: BTreeSet::from([prefix(192, 168, 1, 1, 24)]),
                dhcp_client: false,
            },
        )]),
        stp: None,
        ..DesiredState::default()
    }
}

fn active() -> Vlan {
    Vlan {
        name: None,
        active: true,
    }
}

fn run(net: &mut FakeNet, desired: &DesiredState) -> Vec<Op> {
    let ops = reconcile(net, desired, &mut |_| {}).expect("reconcile");
    let actual = net.observe().unwrap();
    assert_eq!(plan(desired, &actual).unwrap(), vec![], "not idempotent");
    assert_eq!(actual.platform_ops(), vec![]);
    ops
}

fn position(ops: &[Op], wanted: &Op) -> usize {
    ops.iter()
        .position(|op| op == wanted)
        .unwrap_or_else(|| panic!("{wanted} missing in {ops:#?}"))
}

fn assert_factory_default_applied(net: &FakeNet) {
    let s = &net.state;
    assert_eq!(
        s.links[BRIDGE_NAME].kind,
        LinkKind::Bridge {
            vlan_filtering: true,
            default_pvid: 0,
            mst_enabled: true,
            stp: BridgeStp::Off,
        }
    );
    assert!(s.links[BRIDGE_NAME].up);
    assert!(s.links["eth0"].up && s.links["lo"].up);
    for i in 1..=8 {
        let port = format!("lan{i}");
        assert_eq!(s.links[&port].master.as_deref(), Some(BRIDGE_NAME));
        assert!(s.links[&port].up);
        assert_eq!(s.bridge_vlans[&port], BTreeMap::from([(1, ACCESS)]));
    }
    assert_eq!(s.bridge_vlans[BRIDGE_NAME], BTreeMap::from([(1, TAGGED)]));
    assert_eq!(
        s.links["vlan1"].kind,
        LinkKind::Vlan {
            parent: BRIDGE_NAME.into(),
            id: 1
        }
    );
    assert!(s.links["vlan1"].up);
    assert_eq!(
        s.addresses["vlan1"],
        BTreeSet::from([prefix(192, 168, 1, 1, 24)])
    );
}

#[test]
fn factory_default_on_fresh_boot() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());
    assert_factory_default_applied(&net);
}

#[test]
fn second_run_changes_nothing() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());
    assert_eq!(run(&mut net, &factory_default()), vec![]);
}

#[test]
fn migration_from_static_network_script() {
    // What /etc/init.d/rtl83xx-network leaves behind: a bridge without VLAN
    // filtering, default PVID 1 on all ports, the address on the bridge.
    let mut net = FakeNet::gs1900_8();
    let s = &mut net.state;
    s.links.insert(
        BRIDGE_NAME.into(),
        Link {
            kind: LinkKind::Bridge {
                vlan_filtering: false,
                default_pvid: 1,
                mst_enabled: false,
                stp: BridgeStp::Off,
            },
            up: true,
            master: None,
        },
    );
    for (name, l) in s.links.iter_mut() {
        l.up = true;
        if name.starts_with("lan") {
            l.master = Some(BRIDGE_NAME.into());
            s.bridge_vlans
                .insert(name.clone(), BTreeMap::from([(1, ACCESS)]));
        }
    }
    s.addresses.insert(
        BRIDGE_NAME.into(),
        BTreeSet::from([prefix(192, 168, 1, 1, 24)]),
    );

    let ops = run(&mut net, &factory_default());

    assert_factory_default_applied(&net);
    assert!(net
        .state
        .addresses
        .get(BRIDGE_NAME)
        .is_none_or(|a| a.is_empty()));
    // The management address never disappears in between.
    let added = position(
        &ops,
        &Op::AddAddress {
            dev: "vlan1".into(),
            prefix: prefix(192, 168, 1, 1, 24),
        },
    );
    let removed = position(
        &ops,
        &Op::DelAddress {
            dev: BRIDGE_NAME.into(),
            prefix: prefix(192, 168, 1, 1, 24),
        },
    );
    assert!(added < removed);
}

#[test]
fn unconfigured_port_leaves_the_bridge() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());

    let mut desired = factory_default();
    desired.ports.remove("lan8");
    run(&mut net, &desired);

    let lan8 = &net.state.links["lan8"];
    assert_eq!(lan8.master, None);
    assert!(!lan8.up);
    assert!(!net.state.bridge_vlans.contains_key("lan8"));
}

#[test]
fn disabled_port_stays_in_the_bridge() {
    let mut net = FakeNet::gs1900_8();
    let mut desired = factory_default();
    desired.ports.get_mut("lan2").unwrap().enabled = false;
    run(&mut net, &desired);

    let lan2 = &net.state.links["lan2"];
    assert_eq!(lan2.master.as_deref(), Some(BRIDGE_NAME));
    assert!(!lan2.up);
}

#[test]
fn access_vlan_change() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());

    let mut desired = factory_default();
    desired.ports.get_mut("lan3").unwrap().native_vlan = Some(20);
    let ops = run(&mut net, &desired);

    assert_eq!(
        ops,
        vec![
            Op::AddBridgeVlan {
                dev: "lan3".into(),
                vid: 20,
                flags: ACCESS
            },
            Op::DelBridgeVlan {
                dev: "lan3".into(),
                vid: 1
            },
        ]
    );
    assert_eq!(
        net.state.bridge_vlans["lan3"],
        BTreeMap::from([(20, ACCESS)])
    );
}

#[test]
fn management_address_change() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());

    let mut desired = factory_default();
    desired.svis.get_mut("vlan1").unwrap().addresses = BTreeSet::from([prefix(10, 0, 0, 2, 8)]);
    let ops = run(&mut net, &desired);

    assert_eq!(
        ops,
        vec![
            Op::AddAddress {
                dev: "vlan1".into(),
                prefix: prefix(10, 0, 0, 2, 8)
            },
            Op::DelAddress {
                dev: "vlan1".into(),
                prefix: prefix(192, 168, 1, 1, 24)
            },
        ]
    );
}

#[test]
fn svi_moves_to_another_vlan() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());

    let mut desired = factory_default();
    let svi = desired.svis.remove("vlan1").unwrap();
    desired.svis.insert("mgmt".into(), Svi { vlan: 99, ..svi });
    run(&mut net, &desired);

    let s = &net.state;
    assert!(!s.links.contains_key("vlan1"));
    assert_eq!(
        s.links["mgmt"].kind,
        LinkKind::Vlan {
            parent: BRIDGE_NAME.into(),
            id: 99
        }
    );
    assert_eq!(s.bridge_vlans[BRIDGE_NAME], BTreeMap::from([(99, TAGGED)]));
}

#[test]
fn svi_name_taken_by_a_foreign_link() {
    let mut net = FakeNet::gs1900_8();
    let mut desired = factory_default();
    let svi = desired.svis.remove("vlan1").unwrap();
    desired.svis.insert("eth0".into(), svi);

    let err = reconcile(&mut net, &desired, &mut |_| {}).unwrap_err();
    assert!(err.0.contains("eth0"), "{err}");
    // Nothing touched beyond the platform basics.
    assert!(!net.state.links.contains_key(BRIDGE_NAME));
}

#[test]
fn empty_configuration_isolates_all_ports() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());
    run(&mut net, &DesiredState::default());

    let s = &net.state;
    assert!(!s.links.contains_key("vlan1"));
    for i in 1..=8 {
        let port = &s.links[&format!("lan{i}")];
        assert_eq!(port.master, None);
        assert!(!port.up);
    }
    // The conduit stays up, so a later commit works without a restart.
    assert!(s.links["eth0"].up);
}

#[test]
fn access_port_becomes_trunk() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());

    let mut desired = factory_default();
    desired.vlans.extend([(10, active()), (20, active())]);
    desired.ports.insert(
        "lan2".into(),
        Port {
            enabled: true,
            native_vlan: Some(1),
            tagged_vlans: BTreeSet::from([10, 20]),
        },
    );
    run(&mut net, &desired);

    assert_eq!(
        net.state.bridge_vlans["lan2"],
        BTreeMap::from([(1, ACCESS), (10, TAGGED), (20, TAGGED)])
    );
    // Only SVI VLANs reach the CPU.
    assert_eq!(
        net.state.bridge_vlans[BRIDGE_NAME],
        BTreeMap::from([(1, TAGGED)])
    );
}

#[test]
fn native_vlan_change_keeps_a_pvid() {
    let mut net = FakeNet::gs1900_8();
    let mut desired = factory_default();
    desired.vlans.extend([(10, active()), (20, active())]);
    desired.ports.insert(
        "lan2".into(),
        Port {
            enabled: true,
            native_vlan: Some(1),
            tagged_vlans: BTreeSet::from([10, 20]),
        },
    );
    run(&mut net, &desired);

    // Native 1 -> 10, and 1 becomes tagged.
    let before = net.clone();
    let port = desired.ports.get_mut("lan2").unwrap();
    port.native_vlan = Some(10);
    port.tagged_vlans = BTreeSet::from([1, 20]);
    let ops = run(&mut net, &desired);

    // Replayed step by step, the port has a PVID after every operation.
    let mut replay = before;
    for op in &ops {
        replay.apply(op).unwrap();
        assert!(
            replay.state.bridge_vlans["lan2"].values().any(|f| f.pvid),
            "no PVID after {op}"
        );
    }
    assert_eq!(
        net.state.bridge_vlans["lan2"],
        BTreeMap::from([(1, TAGGED), (10, ACCESS), (20, TAGGED)])
    );
}

#[test]
fn suspended_vlan_has_no_bridge_entries() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());

    // What switch-model makes of VLAN 1 suspended: no port carries it, the
    // SVI stays.
    let mut desired = factory_default();
    desired.vlans.get_mut(&1).unwrap().active = false;
    for port in desired.ports.values_mut() {
        port.native_vlan = None;
    }
    run(&mut net, &desired);

    let s = &net.state;
    assert!(s.bridge_vlans.get("lan1").is_none_or(|v| v.is_empty()));
    assert!(s.bridge_vlans.get(BRIDGE_NAME).is_none_or(|v| v.is_empty()));
    assert!(s.links.contains_key("vlan1"));
    assert_eq!(s.links["lan1"].master.as_deref(), Some(BRIDGE_NAME));
}

#[test]
fn switch_to_port_based_groups() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());

    // lan1..4 in group 1 with the management SVI, lan5..8 in group 2.
    let mut desired = factory_default();
    desired.mode = VlanMode::PortBased;
    desired.vlans = BTreeMap::from([(1, active()), (2, active())]);
    for i in 5..=8 {
        desired.ports.insert(format!("lan{i}"), Port::access(2));
    }
    run(&mut net, &desired);

    let s = &net.state;
    for i in 1..=4 {
        assert_eq!(
            s.bridge_vlans[&format!("lan{i}")],
            BTreeMap::from([(1, ACCESS)])
        );
    }
    for i in 5..=8 {
        assert_eq!(
            s.bridge_vlans[&format!("lan{i}")],
            BTreeMap::from([(2, ACCESS)])
        );
    }
    assert_eq!(s.bridge_vlans[BRIDGE_NAME], BTreeMap::from([(1, TAGGED)]));
    assert_eq!(
        s.addresses["vlan1"],
        BTreeSet::from([prefix(192, 168, 1, 1, 24)])
    );
}

// ---------------------------------------------------------------------------
// DHCP addresses
// ---------------------------------------------------------------------------

/// The factory default with the DHCP client on vlan1, applied, and a lease
/// address added the way the udhcpc script adds it.
fn with_dhcp_lease() -> (FakeNet, DesiredState) {
    let mut net = FakeNet::gs1900_8();
    let mut desired = factory_default();
    desired.svis.get_mut("vlan1").unwrap().dhcp_client = true;
    run(&mut net, &desired);
    net.state
        .dhcp_addresses
        .entry("vlan1".into())
        .or_default()
        .insert(prefix(10, 99, 0, 100, 24), 3600);
    (net, desired)
}

#[test]
fn dhcp_address_stays_while_the_client_runs() {
    let (mut net, desired) = with_dhcp_lease();
    assert_eq!(run(&mut net, &desired), vec![]);
    assert!(net.state.dhcp_addresses["vlan1"].contains_key(&prefix(10, 99, 0, 100, 24)));
    assert_eq!(
        net.state.addresses["vlan1"],
        BTreeSet::from([prefix(192, 168, 1, 1, 24)])
    );
}

#[test]
fn dhcp_address_goes_with_the_client() {
    let (mut net, mut desired) = with_dhcp_lease();
    desired.svis.get_mut("vlan1").unwrap().dhcp_client = false;
    let ops = run(&mut net, &desired);
    assert_eq!(
        ops,
        vec![Op::DelAddress {
            dev: "vlan1".into(),
            prefix: prefix(10, 99, 0, 100, 24)
        }]
    );
    assert!(net.state.dhcp_addresses["vlan1"].is_empty());
    assert_eq!(
        net.state.addresses["vlan1"],
        BTreeSet::from([prefix(192, 168, 1, 1, 24)])
    );
}

#[test]
fn static_address_replaces_the_same_dhcp_address() {
    let (mut net, mut desired) = with_dhcp_lease();
    desired
        .svis
        .get_mut("vlan1")
        .unwrap()
        .addresses
        .insert(prefix(10, 99, 0, 100, 24));
    let ops = run(&mut net, &desired);
    let dhcp = prefix(10, 99, 0, 100, 24);
    assert!(
        position(
            &ops,
            &Op::DelAddress {
                dev: "vlan1".into(),
                prefix: dhcp
            }
        ) < position(
            &ops,
            &Op::AddAddress {
                dev: "vlan1".into(),
                prefix: dhcp
            }
        )
    );
    assert!(net.state.addresses["vlan1"].contains(&dhcp));
    assert!(net.state.dhcp_addresses["vlan1"].is_empty());
}

#[test]
fn dhcp_address_on_a_port_is_removed() {
    let (mut net, desired) = with_dhcp_lease();
    net.state
        .dhcp_addresses
        .entry("lan1".into())
        .or_default()
        .insert(prefix(10, 99, 0, 101, 24), 60);
    let ops = run(&mut net, &desired);
    assert_eq!(
        ops,
        vec![Op::DelAddress {
            dev: "lan1".into(),
            prefix: prefix(10, 99, 0, 101, 24)
        }]
    );
}

/// The factory default with RSTP, all defaults.
fn with_rstp() -> DesiredState {
    let json = r#"{"openconfig-spanning-tree:stp": {"global": {"config":
        {"enabled-protocol": ["openconfig-spanning-tree-types:RSTP"]}}}}"#;
    let config = switch_model::Config::from_json(json).unwrap();
    let ports = (1..=8).map(|i| format!("lan{i}")).collect();
    let mut desired = factory_default();
    desired.stp = switch_model::desired_state(&config, &ports).unwrap().stp;
    assert!(desired.stp.is_some());
    desired
}

fn bridge_stp(net: &FakeNet) -> BridgeStp {
    match &net.state.links[BRIDGE_NAME].kind {
        LinkKind::Bridge { stp, .. } => *stp,
        other => panic!("not a bridge: {other:?}"),
    }
}

#[test]
fn spanning_tree_is_on_before_ports_join() {
    let mut net = FakeNet::gs1900_8();
    let desired = with_rstp();
    let ops = run(&mut net, &desired);
    assert_eq!(bridge_stp(&net), BridgeStp::User);
    let stp_on = position(
        &ops,
        &Op::SetBridgeStp {
            name: BRIDGE_NAME.into(),
            on: true,
        },
    );
    let first_port = position(
        &ops,
        &Op::SetMaster {
            name: "lan1".into(),
            master: Some(BRIDGE_NAME.into()),
        },
    );
    assert!(stp_on < first_port);
}

#[test]
fn spanning_tree_off_and_on_again() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &with_rstp());
    let ops = run(&mut net, &factory_default());
    assert_eq!(
        ops,
        vec![Op::SetBridgeStp {
            name: BRIDGE_NAME.into(),
            on: false
        }]
    );
    assert_eq!(bridge_stp(&net), BridgeStp::Off);
    let ops = run(&mut net, &with_rstp());
    assert_eq!(
        ops,
        vec![Op::SetBridgeStp {
            name: BRIDGE_NAME.into(),
            on: true
        }]
    );
}

#[test]
fn kernel_stp_is_replaced() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &with_rstp());
    if let LinkKind::Bridge { stp, .. } = &mut net.state.links.get_mut(BRIDGE_NAME).unwrap().kind {
        *stp = BridgeStp::Kernel;
    }
    let off = Op::SetBridgeStp {
        name: BRIDGE_NAME.into(),
        on: false,
    };
    let on = Op::SetBridgeStp {
        name: BRIDGE_NAME.into(),
        on: true,
    };
    assert_eq!(run(&mut net, &with_rstp()), vec![off.clone(), on]);
    net.state.links.get_mut(BRIDGE_NAME).unwrap().kind = LinkKind::Bridge {
        vlan_filtering: true,
        default_pvid: 0,
        mst_enabled: true,
        stp: BridgeStp::Kernel,
    };
    assert_eq!(run(&mut net, &factory_default()), vec![off]);
}

#[test]
fn bridge_without_mst_and_with_port_vlans_is_recreated() {
    let mut net = FakeNet::gs1900_8();
    run(&mut net, &factory_default());
    net.state.links.get_mut(BRIDGE_NAME).unwrap().kind = LinkKind::Bridge {
        vlan_filtering: true,
        default_pvid: 0,
        mst_enabled: false,
        stp: BridgeStp::Off,
    };
    let ops = run(&mut net, &factory_default());
    assert_eq!(
        ops[..2],
        [
            Op::DeleteLink {
                name: BRIDGE_NAME.into()
            },
            Op::CreateBridge {
                name: BRIDGE_NAME.into()
            }
        ]
    );
    assert_factory_default_applied(&net);
}
