//! BRIDGE-MIB (RFC 4188), Q-BRIDGE-MIB (RFC 4363) and RSTP-MIB (RFC 4318)
//! as state data, for clixon_snmp.
//!
//! The modules are translations of the MIBs to YANG (RFC 6643, see
//! scripts/mib-to-yang.sh), in which every object is config false. So they
//! are another view of the state of the switch, built from the configuration
//! in effect, the kernel and mstpd, never configuration.
//!
//! Ports are numbered by [`port_numbers`], which is also the numbering of
//! port lists. VLANs are their own filtering databases (FDB id = VLAN id), as
//! in the kernel bridge.
//!
//! Left out: the source route and transparent bridge counters
//! (dot1dTpPortTable), static FDB tables, GVRP/GMRP, multicast tables, VLAN
//! statistics. Spanning tree objects describe the CIST: MSTIs have no MIB
//! here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::{escape, BridgeId, DesiredState, EdgePort, StpProtocol, StpState};

const BRIDGE_MIB_NS: &str = "urn:ietf:params:xml:ns:yang:smiv2:BRIDGE-MIB";
const Q_BRIDGE_MIB_NS: &str = "urn:ietf:params:xml:ns:yang:smiv2:Q-BRIDGE-MIB";
const RSTP_MIB_NS: &str = "urn:ietf:params:xml:ns:yang:smiv2:RSTP-MIB";

/// Largest value of an int32 path cost in dot1dStpPortPathCost.
const MAX_PATH_COST_16: u32 = 65535;

/// The MIB modules a state request asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MibModules {
    pub bridge: bool,
    pub q_bridge: bool,
    pub rstp: bool,
}

impl MibModules {
    /// From the XPath of a state request, e.g.
    /// "q-bridge:Q-BRIDGE-MIB/q-bridge:dot1qTpFdbTable": the modules of its
    /// first step. None of them for "/" or no XPath: other clients than
    /// clixon_snmp do not get the MIBs unless they ask.
    pub fn from_xpath(xpath: Option<&str>) -> Self {
        let first = xpath
            .unwrap_or_default()
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or_default();
        let name = first.rsplit(':').next().unwrap_or(first);
        let name = name.split('[').next().unwrap_or(name);
        MibModules {
            bridge: name == "BRIDGE-MIB",
            q_bridge: name == "Q-BRIDGE-MIB",
            rstp: name == "RSTP-MIB",
        }
    }

    pub fn any(self) -> bool {
        self.bridge || self.q_bridge || self.rstp
    }
}

/// A port's VLANs as the kernel bridge has them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortVlans {
    pub pvid: Option<u16>,
    /// VLAN id, and whether egress is untagged.
    pub vlans: BTreeMap<u16, bool>,
}

/// An entry of the bridge's forwarding database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdbEntry {
    pub mac: [u8; 6],
    /// None: the entry has no VLAN.
    pub vlan: Option<u16>,
    /// The port, or the bridge itself.
    pub port: String,
    pub status: FdbStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdbStatus {
    /// Learned from traffic.
    Learned,
    /// An address of the switch itself.
    Own,
    /// Added by management (static).
    Mgmt,
}

impl FdbStatus {
    fn as_str(self) -> &'static str {
        match self {
            FdbStatus::Learned => "learned",
            FdbStatus::Own => "self",
            FdbStatus::Mgmt => "mgmt",
        }
    }
}

/// Everything the MIBs are built from.
#[derive(Debug, Clone, Copy)]
pub struct BridgeInfo<'a> {
    pub applied: &'a DesiredState,
    pub bridge_mac: Option<[u8; 6]>,
    /// Kernel ifindex by port, IF-MIB's ifIndex.
    pub ifindex: &'a BTreeMap<String, u32>,
    /// Kernel bridge VLANs by port.
    pub port_vlans: &'a BTreeMap<String, PortVlans>,
    /// None: not read for this request.
    pub fdb: Option<&'a [FdbEntry]>,
    /// What mstpd reports, while spanning tree runs; None otherwise or not
    /// read for this request.
    pub stp: Option<&'a StpState>,
}

/// Port numbers 1..n: ports in the order of the number at the end of their
/// names ("lan2" before "lan10"), then by name, names without a number last.
pub fn port_numbers<'a>(ports: impl IntoIterator<Item = &'a String>) -> BTreeMap<String, u16> {
    let mut sorted: Vec<&String> = ports.into_iter().collect();
    let trailing = |name: &str| {
        let digits = name.len() - name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        name[name.len() - digits..].parse::<u32>().ok()
    };
    // Names without a number last.
    sorted.sort_by_key(|name| (trailing(name).is_none(), trailing(name), *name));
    sorted
        .into_iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), u16::try_from(i + 1).unwrap_or(u16::MAX)))
        .collect()
}

/// State data XML of the requested MIB modules.
pub fn bridge_mib_xml(info: &BridgeInfo, modules: MibModules) -> String {
    let mut xml = String::new();
    let numbers = port_numbers(info.applied.ports.keys());
    if modules.bridge {
        bridge_xml(&mut xml, info, &numbers);
    }
    if modules.rstp {
        rstp_xml(&mut xml, info);
    }
    if modules.q_bridge {
        q_bridge_xml(&mut xml, info, &numbers);
    }
    xml
}

fn mac_text(mac: &[u8; 6]) -> String {
    crate::colon_hex(mac)
}

/// dot1dBase, the port table with Q-BRIDGE-MIB's per-port VLAN settings,
/// dot1dStp with RSTP-MIB's port settings, and dot1dTpFdbTable.
fn bridge_xml(xml: &mut String, info: &BridgeInfo, numbers: &BTreeMap<String, u16>) {
    let _ = write!(xml, r#"<BRIDGE-MIB xmlns="{BRIDGE_MIB_NS}"><dot1dBase>"#);
    if let Some(mac) = &info.bridge_mac {
        let _ = write!(
            xml,
            "<dot1dBaseBridgeAddress>{}</dot1dBaseBridgeAddress>",
            mac_text(mac)
        );
    }
    let _ = write!(
        xml,
        "<dot1dBaseNumPorts>{}</dot1dBaseNumPorts><dot1dBaseType>transparent-only</dot1dBaseType></dot1dBase>",
        numbers.len()
    );

    let stp = info.applied.stp.as_ref().filter(|_| info.stp.is_some());
    if let (Some(stp), Some(state)) = (stp, info.stp) {
        let bridge = state.cist.bridge.as_ref();
        let _ = write!(
            xml,
            "<dot1dStp><dot1dStpProtocolSpecification>ieee8021d</dot1dStpProtocolSpecification><dot1dStpPriority>{}</dot1dStpPriority>",
            stp.cist.bridge_priority
        );
        if let Some(t) = bridge.and_then(|b| b.time_since_topology_change) {
            let _ = write!(
                xml,
                "<dot1dStpTimeSinceTopologyChange>{}</dot1dStpTimeSinceTopologyChange>",
                u64::from(t) * 100
            );
        }
        if let Some(n) = bridge.and_then(|b| b.topology_changes) {
            let _ = write!(
                xml,
                "<dot1dStpTopChanges>{}</dot1dStpTopChanges>",
                n % (1 << 32)
            );
        }
        if let Some(root) = bridge.and_then(|b| b.root.as_ref()) {
            let _ = write!(
                xml,
                "<dot1dStpDesignatedRoot>{}</dot1dStpDesignatedRoot>",
                bridge_id(root)
            );
        }
        if let Some(cost) = bridge.and_then(|b| b.root_cost) {
            let _ = write!(xml, "<dot1dStpRootCost>{}</dot1dStpRootCost>", int32(cost));
        }
        let root_port = bridge
            .and_then(|b| b.root_port.as_ref())
            .and_then(|p| numbers.get(p))
            .copied()
            .unwrap_or(0);
        let _ = write!(
            xml,
            "<dot1dStpRootPort>{root_port}</dot1dStpRootPort>\
             <dot1dStpMaxAge>{ma}</dot1dStpMaxAge><dot1dStpHelloTime>{ht}</dot1dStpHelloTime>\
             <dot1dStpHoldTime>100</dot1dStpHoldTime><dot1dStpForwardDelay>{fd}</dot1dStpForwardDelay>\
             <dot1dStpBridgeMaxAge>{ma}</dot1dStpBridgeMaxAge><dot1dStpBridgeHelloTime>{ht}</dot1dStpBridgeHelloTime>\
             <dot1dStpBridgeForwardDelay>{fd}</dot1dStpBridgeForwardDelay></dot1dStp>",
            ma = u32::from(stp.max_age) * 100,
            ht = u32::from(stp.hello_time) * 100,
            fd = u32::from(stp.forward_delay) * 100,
        );
    }

    xml.push_str("<dot1dBasePortTable>");
    for (port, number) in numbers {
        let _ = write!(
            xml,
            "<dot1dBasePortEntry><dot1dBasePort>{number}</dot1dBasePort>"
        );
        if let Some(index) = info.ifindex.get(port) {
            let _ = write!(xml, "<dot1dBasePortIfIndex>{index}</dot1dBasePortIfIndex>");
        }
        xml.push_str("<dot1dBasePortCircuit>0.0</dot1dBasePortCircuit>");
        port_vlan_xml(xml, info, port);
        xml.push_str("</dot1dBasePortEntry>");
    }
    xml.push_str("</dot1dBasePortTable>");

    if let (Some(stp), Some(state)) = (stp, info.stp) {
        xml.push_str("<dot1dStpPortTable>");
        for (port, number) in numbers {
            let tree_port = stp.cist.ports.get(port).copied().unwrap_or_default();
            let s = state.cist.ports.get(port);
            let features = stp.ports.get(port).copied().unwrap_or_default();
            let enabled = s.and_then(|s| s.port_state) != Some("DISABLED");
            let _ = write!(
                xml,
                "<dot1dStpPortEntry><dot1dStpPort>{number}</dot1dStpPort><dot1dStpPortPriority>{}</dot1dStpPortPriority>",
                tree_port.priority
            );
            if let Some(port_state) = s.and_then(|s| s.port_state) {
                let _ = write!(
                    xml,
                    "<dot1dStpPortState>{}</dot1dStpPortState>",
                    match port_state {
                        "BLOCKING" => "blocking",
                        "LEARNING" => "learning",
                        "FORWARDING" => "forwarding",
                        _ => "disabled",
                    }
                );
            }
            let _ = write!(
                xml,
                "<dot1dStpPortEnable>{}</dot1dStpPortEnable>",
                if enabled { "enabled" } else { "disabled" }
            );
            let cost = s.and_then(|s| s.path_cost).or(tree_port.cost);
            if let Some(cost) = cost {
                let _ = write!(
                    xml,
                    "<dot1dStpPortPathCost>{}</dot1dStpPortPathCost>",
                    cost.min(MAX_PATH_COST_16)
                );
            }
            if let Some(s) = s {
                if let Some(root) = &s.designated_root {
                    let _ = write!(
                        xml,
                        "<dot1dStpPortDesignatedRoot>{}</dot1dStpPortDesignatedRoot>",
                        bridge_id(root)
                    );
                }
                if let Some(cost) = s.designated_cost {
                    let _ = write!(
                        xml,
                        "<dot1dStpPortDesignatedCost>{}</dot1dStpPortDesignatedCost>",
                        int32(cost)
                    );
                }
                if let Some(bridge) = &s.designated_bridge {
                    let _ = write!(
                        xml,
                        "<dot1dStpPortDesignatedBridge>{}</dot1dStpPortDesignatedBridge>",
                        bridge_id(bridge)
                    );
                }
                if let (Some(priority), Some(num)) =
                    (s.designated_port_priority, s.designated_port_num)
                {
                    let id = (u16::from(priority / 16) << 12) | (num & 0x0fff);
                    let _ = write!(
                        xml,
                        "<dot1dStpPortDesignatedPort>{}</dot1dStpPortDesignatedPort>",
                        base64(&id.to_be_bytes())
                    );
                }
                if let Some(n) = s.forward_transitions {
                    let _ = write!(
                        xml,
                        "<dot1dStpPortForwardTransitions>{}</dot1dStpPortForwardTransitions>",
                        n % (1 << 32)
                    );
                }
            }
            if let Some(cost) = cost {
                let _ = write!(
                    xml,
                    "<dot1dStpPortPathCost32>{}</dot1dStpPortPathCost32>",
                    int32(cost)
                );
            }
            // RSTP-MIB's dot1dStpExtPortTable.
            let _ = write!(
                xml,
                r#"<dot1dStpPortProtocolMigration xmlns="{RSTP_MIB_NS}">false</dot1dStpPortProtocolMigration><dot1dStpPortAdminEdgePort xmlns="{RSTP_MIB_NS}">{}</dot1dStpPortAdminEdgePort>"#,
                features.edge == EdgePort::Enable
            );
            if let Some(edge) = s.and_then(|s| s.oper_edge) {
                let _ = write!(
                    xml,
                    r#"<dot1dStpPortOperEdgePort xmlns="{RSTP_MIB_NS}">{edge}</dot1dStpPortOperEdgePort>"#
                );
            }
            let _ = write!(
                xml,
                r#"<dot1dStpPortAdminPointToPoint xmlns="{RSTP_MIB_NS}">{}</dot1dStpPortAdminPointToPoint>"#,
                match features.point_to_point {
                    Some(true) => "forceTrue",
                    Some(false) => "forceFalse",
                    None => "auto",
                }
            );
            if let Some(p2p) = s.and_then(|s| s.oper_point_to_point) {
                let _ = write!(
                    xml,
                    r#"<dot1dStpPortOperPointToPoint xmlns="{RSTP_MIB_NS}">{p2p}</dot1dStpPortOperPointToPoint>"#
                );
            }
            let _ = write!(
                xml,
                r#"<dot1dStpPortAdminPathCost xmlns="{RSTP_MIB_NS}">{}</dot1dStpPortAdminPathCost></dot1dStpPortEntry>"#,
                tree_port.cost.map_or(0, int32)
            );
        }
        xml.push_str("</dot1dStpPortTable>");
    }

    if let Some(fdb) = info.fdb {
        // One entry per address: the table has no VLAN.
        let mut seen = BTreeSet::new();
        xml.push_str("<dot1dTpFdbTable>");
        for entry in fdb {
            if seen.insert(entry.mac) {
                let _ = write!(
                    xml,
                    "<dot1dTpFdbEntry><dot1dTpFdbAddress>{}</dot1dTpFdbAddress><dot1dTpFdbPort>{}</dot1dTpFdbPort><dot1dTpFdbStatus>{}</dot1dTpFdbStatus></dot1dTpFdbEntry>",
                    mac_text(&entry.mac),
                    numbers.get(&entry.port).copied().unwrap_or(0),
                    entry.status.as_str()
                );
            }
        }
        xml.push_str("</dot1dTpFdbTable>");
    }
    xml.push_str("</BRIDGE-MIB>");
}

/// Q-BRIDGE-MIB's dot1qPortVlanTable, which augments dot1dBasePortEntry.
fn port_vlan_xml(xml: &mut String, info: &BridgeInfo, port: &str) {
    let vlans = info.port_vlans.get(port);
    let pvid = vlans.and_then(|v| v.pvid);
    if let Some(pvid) = pvid {
        let _ = write!(
            xml,
            r#"<dot1qPvid xmlns="{Q_BRIDGE_MIB_NS}">{pvid}</dot1qPvid>"#
        );
    }
    let _ = write!(
        xml,
        r#"<dot1qPortAcceptableFrameTypes xmlns="{Q_BRIDGE_MIB_NS}">{}</dot1qPortAcceptableFrameTypes><dot1qPortIngressFiltering xmlns="{Q_BRIDGE_MIB_NS}">true</dot1qPortIngressFiltering><dot1qPortGvrpStatus xmlns="{Q_BRIDGE_MIB_NS}">disabled</dot1qPortGvrpStatus>"#,
        if pvid.is_some() {
            "admitAll"
        } else {
            "admitOnlyVlanTagged"
        }
    );
}

/// RSTP-MIB's dot1dStp scalars: version and transmit hold count.
fn rstp_xml(xml: &mut String, info: &BridgeInfo) {
    let Some(stp) = info.applied.stp.as_ref().filter(|_| info.stp.is_some()) else {
        return;
    };
    let _ = write!(
        xml,
        r#"<RSTP-MIB xmlns="{RSTP_MIB_NS}"><dot1dStp><dot1dStpVersion>{}</dot1dStpVersion><dot1dStpTxHoldCount>{}</dot1dStpTxHoldCount></dot1dStp></RSTP-MIB>"#,
        match stp.protocol {
            StpProtocol::Stp => "stpCompatible",
            // RSTP-MIB has no value for MSTP.
            StpProtocol::Rstp | StpProtocol::Mstp => "rstp",
        },
        stp.hold_count
    );
}

/// dot1qBase, the FDBs, and the VLANs: as configured (static) and as in the
/// kernel (current).
fn q_bridge_xml(xml: &mut String, info: &BridgeInfo, numbers: &BTreeMap<String, u16>) {
    // VLAN -> port -> untagged, from the kernel.
    let mut current: BTreeMap<u16, BTreeMap<&str, bool>> = BTreeMap::new();
    for (port, vlans) in info.port_vlans {
        if !numbers.contains_key(port) {
            continue;
        }
        for (vid, untagged) in &vlans.vlans {
            current.entry(*vid).or_default().insert(port, *untagged);
        }
    }

    let _ = write!(
        xml,
        r#"<Q-BRIDGE-MIB xmlns="{Q_BRIDGE_MIB_NS}"><dot1qBase><dot1qVlanVersionNumber>version1</dot1qVlanVersionNumber><dot1qMaxVlanId>4094</dot1qMaxVlanId><dot1qMaxSupportedVlans>4094</dot1qMaxSupportedVlans><dot1qNumVlans>{}</dot1qNumVlans><dot1qGvrpStatus>disabled</dot1qGvrpStatus></dot1qBase>"#,
        current.len()
    );

    if let Some(fdb) = info.fdb {
        let mut dynamic: BTreeMap<u16, u32> = BTreeMap::new();
        for entry in fdb {
            if let (Some(vlan), FdbStatus::Learned) = (entry.vlan, entry.status) {
                *dynamic.entry(vlan).or_default() += 1;
            }
        }
        xml.push_str("<dot1qFdbTable>");
        for vid in current.keys() {
            let _ = write!(
                xml,
                "<dot1qFdbEntry><dot1qFdbId>{vid}</dot1qFdbId><dot1qFdbDynamicCount>{}</dot1qFdbDynamicCount></dot1qFdbEntry>",
                dynamic.get(vid).copied().unwrap_or(0)
            );
        }
        xml.push_str("</dot1qFdbTable><dot1qTpFdbTable>");
        for entry in fdb {
            let Some(vlan) = entry.vlan else {
                continue;
            };
            let _ = write!(
                xml,
                "<dot1qTpFdbEntry><dot1qFdbId>{vlan}</dot1qFdbId><dot1qTpFdbAddress>{}</dot1qTpFdbAddress><dot1qTpFdbPort>{}</dot1qTpFdbPort><dot1qTpFdbStatus>{}</dot1qTpFdbStatus></dot1qTpFdbEntry>",
                mac_text(&entry.mac),
                numbers.get(&entry.port).copied().unwrap_or(0),
                entry.status.as_str()
            );
        }
        xml.push_str("</dot1qTpFdbTable>");
    }

    xml.push_str("<dot1qVlanCurrentTable>");
    for (vid, ports) in &current {
        let egress = port_list(ports.keys().copied(), numbers);
        let untagged = port_list(ports.iter().filter(|(_, u)| **u).map(|(p, _)| *p), numbers);
        let _ = write!(
            xml,
            "<dot1qVlanCurrentEntry><dot1qVlanTimeMark>0</dot1qVlanTimeMark><dot1qVlanIndex>{vid}</dot1qVlanIndex><dot1qVlanFdbId>{vid}</dot1qVlanFdbId><dot1qVlanCurrentEgressPorts>{egress}</dot1qVlanCurrentEgressPorts><dot1qVlanCurrentUntaggedPorts>{untagged}</dot1qVlanCurrentUntaggedPorts><dot1qVlanStatus>permanent</dot1qVlanStatus><dot1qVlanCreationTime>0</dot1qVlanCreationTime></dot1qVlanCurrentEntry>"
        );
    }
    xml.push_str("</dot1qVlanCurrentTable><dot1qVlanStaticTable>");
    let applied = info.applied;
    let empty = port_list(std::iter::empty(), numbers);
    for (vid, vlan) in &applied.vlans {
        let members = applied.ports.iter().filter(|(_, p)| p.carries(*vid));
        let egress = port_list(members.clone().map(|(n, _)| n.as_str()), numbers);
        let untagged = port_list(
            members
                .filter(|(_, p)| p.native_vlan == Some(*vid))
                .map(|(n, _)| n.as_str()),
            numbers,
        );
        let _ = write!(
            xml,
            "<dot1qVlanStaticEntry><dot1qVlanIndex>{vid}</dot1qVlanIndex><dot1qVlanStaticName>{}</dot1qVlanStaticName><dot1qVlanStaticEgressPorts>{egress}</dot1qVlanStaticEgressPorts><dot1qVlanForbiddenEgressPorts>{empty}</dot1qVlanForbiddenEgressPorts><dot1qVlanStaticUntaggedPorts>{untagged}</dot1qVlanStaticUntaggedPorts><dot1qVlanStaticRowStatus>active</dot1qVlanStaticRowStatus></dot1qVlanStaticEntry>",
            escape(vlan.name.as_deref().unwrap_or_default())
        );
    }
    xml.push_str("</dot1qVlanStaticTable></Q-BRIDGE-MIB>");
}

/// A PortList, base64 as YANG binary: one octet per 8 ports, port 1 in the
/// most significant bit of the first octet.
fn port_list<'a>(ports: impl Iterator<Item = &'a str>, numbers: &BTreeMap<String, u16>) -> String {
    let octets = numbers.len().div_ceil(8).max(1);
    let mut list = vec![0u8; octets];
    for port in ports {
        if let Some(n) = numbers.get(port) {
            let bit = usize::from(*n - 1);
            list[bit / 8] |= 0x80 >> (bit % 8);
        }
    }
    base64(&list)
}

/// A BridgeId, base64: priority (with the system ID extension 0), MAC.
fn bridge_id(id: &BridgeId) -> String {
    let mut octets = id.priority.to_be_bytes().to_vec();
    match crate::parse_mac(&id.address) {
        Some(mac) => octets.extend(mac),
        None => octets.extend([0; 6]),
    }
    base64(&octets)
}

fn int32(value: u32) -> u32 {
    value.min(i32::MAX as u32)
}

/// RFC 4648 base64, as YANG's binary type.
pub(crate) fn base64(octets: &[u8]) -> String {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(octets.len().div_ceil(3) * 4);
    for chunk in octets.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(CHARS[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
