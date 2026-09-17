//! Spanning tree: `/stp` of openconfig-spanning-tree, run by mstpd.
//!
//! `global/config/enabled-protocol` selects the protocol: RSTP, MSTP, or
//! clixon-switch's STP; none turns spanning tree off. STP and RSTP take their
//! bridge and port settings from `/stp/rstp`, MSTP from `/stp/mstp`. The
//! container of the protocol not in use may hold configuration; it is
//! validated but has no effect. `/stp/interfaces` holds per-port features of
//! all protocols. Every switch port takes part in spanning tree, with default
//! settings unless an `interfaces` entry overrides them.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::{escape, opt_int, unprefixed, Error, Scalar, SWITCH_NS};

const STP_NS: &str = "http://openconfig.net/yang/spanning-tree";
/// Declares the prefix of openconfig-spanning-tree-types' identities.
const TYPES_XMLNS: &str = r#"xmlns:oc-stp-types="http://openconfig.net/yang/spanning-tree/types""#;

// ---------------------------------------------------------------------------
// RFC 7951 JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct StpConfig {
    pub global: Option<Global>,
    pub rstp: Option<Rstp>,
    pub mstp: Option<Mstp>,
    pub interfaces: Option<FeatureInterfaces>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Global {
    #[serde(default)]
    pub config: GlobalConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GlobalConfig {
    /// identityrefs, e.g. "openconfig-spanning-tree-types:RSTP".
    #[serde(default)]
    pub enabled_protocol: Vec<String>,
    pub bpdu_guard: Option<bool>,
    pub bpdu_filter: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Rstp {
    #[serde(default)]
    pub config: TimerConfig,
    pub interfaces: Option<TreeInterfaces>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Mstp {
    #[serde(default)]
    pub config: TimerConfig,
    #[serde(rename = "mst-instances")]
    pub mst_instances: Option<MstInstances>,
    /// The CIST's ports (clixon-switch augment).
    #[serde(rename = "clixon-switch:interfaces")]
    pub interfaces: Option<TreeInterfaces>,
}

/// `rstp/config` and `mstp/config`: the timers, the bridge priority (in
/// `mstp/config` a clixon-switch augment), and in MSTP the region.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TimerConfig {
    #[serde(default, deserialize_with = "opt_int")]
    pub hello_time: Option<u8>,
    #[serde(default, deserialize_with = "opt_int")]
    pub max_age: Option<u8>,
    #[serde(default, deserialize_with = "opt_int")]
    pub forwarding_delay: Option<u8>,
    #[serde(default, deserialize_with = "opt_int")]
    pub hold_count: Option<u8>,
    #[serde(
        default,
        deserialize_with = "opt_int",
        alias = "clixon-switch:bridge-priority"
    )]
    pub bridge_priority: Option<u16>,
    pub name: Option<String>,
    #[serde(default, deserialize_with = "opt_int")]
    pub revision: Option<u32>,
    #[serde(default, deserialize_with = "opt_int")]
    pub max_hop: Option<u8>,
}

#[derive(Debug, Default, Deserialize)]
pub struct MstInstances {
    #[serde(default, rename = "mst-instance")]
    pub mst_instance: Vec<MstInstance>,
}

#[derive(Debug, Deserialize)]
pub struct MstInstance {
    #[serde(rename = "mst-id", default, deserialize_with = "opt_int")]
    pub mst_id: Option<u16>,
    #[serde(default)]
    pub config: MstInstanceConfig,
    pub interfaces: Option<TreeInterfaces>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MstInstanceConfig {
    #[serde(default, deserialize_with = "opt_int")]
    pub mst_id: Option<u16>,
    /// VLAN ids, or ranges "x..y".
    #[serde(default)]
    pub vlan: Vec<Scalar>,
    #[serde(default, deserialize_with = "opt_int")]
    pub bridge_priority: Option<u16>,
}

/// Port cost and priority in one spanning tree.
#[derive(Debug, Default, Deserialize)]
pub struct TreeInterfaces {
    #[serde(default)]
    pub interface: Vec<TreeInterface>,
}

#[derive(Debug, Deserialize)]
pub struct TreeInterface {
    pub name: String,
    #[serde(default)]
    pub config: TreeInterfaceConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TreeInterfaceConfig {
    #[serde(default, deserialize_with = "opt_int")]
    pub cost: Option<u32>,
    #[serde(default, deserialize_with = "opt_int")]
    pub port_priority: Option<u8>,
}

/// `/stp/interfaces`: per-port features.
#[derive(Debug, Default, Deserialize)]
pub struct FeatureInterfaces {
    #[serde(default)]
    pub interface: Vec<FeatureInterface>,
}

#[derive(Debug, Deserialize)]
pub struct FeatureInterface {
    pub name: String,
    #[serde(default)]
    pub config: FeatureInterfaceConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct FeatureInterfaceConfig {
    pub edge_port: Option<String>,
    pub link_type: Option<String>,
    pub guard: Option<String>,
    pub bpdu_guard: Option<bool>,
    pub bpdu_filter: Option<bool>,
}

// ---------------------------------------------------------------------------
// Desired state
// ---------------------------------------------------------------------------

/// Bridge defaults of IEEE 802.1D/802.1Q, which mstpd uses too.
pub const DEFAULT_BRIDGE_PRIORITY: u16 = 32768;
pub const DEFAULT_PORT_PRIORITY: u8 = 128;
pub const DEFAULT_HELLO_TIME: u8 = 2;
pub const DEFAULT_MAX_AGE: u8 = 20;
pub const DEFAULT_FORWARD_DELAY: u8 = 15;
pub const DEFAULT_HOLD_COUNT: u8 = 6;
pub const DEFAULT_MAX_HOPS: u8 = 20;

/// Spanning tree as it should run. Present only while a protocol is enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stp {
    pub protocol: StpProtocol,
    /// Always [`DEFAULT_HELLO_TIME`]: mstpd supports no other.
    pub hello_time: u8,
    pub max_age: u8,
    pub forward_delay: u8,
    pub hold_count: u8,
    /// MSTP only; the default otherwise.
    pub max_hops: u8,
    /// MST configuration name; None: mstpd's default, the bridge's MAC.
    pub region_name: Option<String>,
    pub region_revision: u16,
    /// The CIST in MSTP, the only tree in STP and RSTP.
    pub cist: Tree,
    /// MSTIs by MSTID, MSTP only.
    pub mstis: BTreeMap<u16, Msti>,
    /// Every switch port, with its features.
    pub ports: BTreeMap<String, PortFeatures>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StpProtocol {
    Stp,
    Rstp,
    Mstp,
}

impl StpProtocol {
    /// mstpd's name, for `mstpctl setforcevers`.
    pub fn mstpd_name(self) -> &'static str {
        match self {
            StpProtocol::Stp => "stp",
            StpProtocol::Rstp => "rstp",
            StpProtocol::Mstp => "mstp",
        }
    }

    /// The identity in XML, with the prefixes of [`TYPES_XMLNS`] and "sw".
    fn identity(self) -> &'static str {
        match self {
            StpProtocol::Stp => "sw:STP",
            StpProtocol::Rstp => "oc-stp-types:RSTP",
            StpProtocol::Mstp => "oc-stp-types:MSTP",
        }
    }
}

/// Bridge priority and port settings of one spanning tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub bridge_priority: u16,
    /// Every switch port.
    pub ports: BTreeMap<String, TreePort>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreePort {
    /// None: derived from the link speed.
    pub cost: Option<u32>,
    pub priority: u8,
}

impl Default for TreePort {
    fn default() -> Self {
        TreePort {
            cost: None,
            priority: DEFAULT_PORT_PRIORITY,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msti {
    /// VLANs mapped to the MSTI. Taken as configured, declared or not: the
    /// mapping has to match the other bridges of the MST region.
    pub vlans: BTreeSet<u16>,
    pub tree: Tree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortFeatures {
    pub edge: EdgePort,
    /// None: detected from the duplex mode.
    pub point_to_point: Option<bool>,
    /// guard ROOT: the port never becomes root port.
    pub root_guard: bool,
    pub bpdu_guard: bool,
    pub bpdu_filter: bool,
}

impl Default for PortFeatures {
    fn default() -> Self {
        PortFeatures {
            edge: EdgePort::Auto,
            point_to_point: None,
            root_guard: false,
            bpdu_guard: false,
            bpdu_filter: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgePort {
    Enable,
    Disable,
    /// Edge once no BPDU has arrived for a while.
    Auto,
}

impl EdgePort {
    /// The identity in XML, with the prefix of [`TYPES_XMLNS`].
    fn identity(self) -> &'static str {
        match self {
            EdgePort::Enable => "oc-stp-types:EDGE_ENABLE",
            EdgePort::Disable => "oc-stp-types:EDGE_DISABLE",
            EdgePort::Auto => "oc-stp-types:EDGE_AUTO",
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validates `/stp` against the configured switch ports, and returns what
/// should run: None while no protocol is enabled.
pub(crate) fn desired_stp(
    config: Option<&StpConfig>,
    ports: &BTreeSet<String>,
    errors: &mut Vec<Error>,
) -> Option<Stp> {
    let default = StpConfig::default();
    let config = config.unwrap_or(&default);
    let mut err = |message: String| errors.push(Error::global(format!("stp: {message}")));

    let global = config.global.as_ref().map(|g| &g.config);
    let mut protocols = BTreeSet::new();
    for identity in global.map_or(&[][..], |g| &g.enabled_protocol) {
        match unprefixed(identity) {
            "STP" => protocols.insert(StpProtocol::Stp),
            "RSTP" => protocols.insert(StpProtocol::Rstp),
            "MSTP" => protocols.insert(StpProtocol::Mstp),
            other => {
                err(format!(
                    "enabled-protocol {other} is not supported, only RSTP, MSTP and STP"
                ));
                continue;
            }
        };
    }
    if protocols.len() > 1 {
        err("enabled-protocol: only one protocol may be enabled".into());
    }

    let features = port_features(config, global, ports, &mut err);

    let rstp = config.rstp.as_ref();
    let rstp_config = rstp.map(|r| &r.config);
    let rstp_timers = timers(rstp_config, "rstp", &mut err);
    if let Some(c) = rstp_config {
        if c.name.is_some() || c.revision.is_some() || c.max_hop.is_some() {
            err("rstp: name, revision and max-hop are MSTP settings".into());
        }
    }
    let rstp_tree = tree(
        rstp_config.and_then(|c| c.bridge_priority),
        rstp.and_then(|r| r.interfaces.as_ref()),
        ports,
        "rstp",
        &mut err,
    );

    let mstp = config.mstp.as_ref();
    let mstp_config = mstp.map(|m| &m.config);
    let mstp_timers = timers(mstp_config, "mstp", &mut err);
    let region_name = mstp_config.and_then(|c| c.name.clone());
    let region_revision = match mstp_config.and_then(|c| c.revision) {
        None => 0,
        Some(r) => u16::try_from(r).unwrap_or_else(|_| {
            err(format!("mstp: revision {r} is out of range 0..65535"));
            0
        }),
    };
    let max_hops = match mstp_config.and_then(|c| c.max_hop) {
        None => DEFAULT_MAX_HOPS,
        Some(h @ 6..=40) => h,
        Some(h) => {
            err(format!("mstp: max-hop {h} is out of range 6..40"));
            DEFAULT_MAX_HOPS
        }
    };
    let cist = tree(
        mstp_config.and_then(|c| c.bridge_priority),
        mstp.and_then(|m| m.interfaces.as_ref()),
        ports,
        "mstp",
        &mut err,
    );
    let mstis = mstis(mstp.and_then(|m| m.mst_instances.as_ref()), ports, &mut err);

    let protocol = protocols.into_iter().next()?;
    let (timers, cist, mstis, max_hops, region_name, region_revision) = match protocol {
        StpProtocol::Mstp => (
            mstp_timers,
            cist,
            mstis,
            max_hops,
            region_name,
            region_revision,
        ),
        _ => (
            rstp_timers,
            rstp_tree,
            BTreeMap::new(),
            DEFAULT_MAX_HOPS,
            None,
            0,
        ),
    };
    Some(Stp {
        protocol,
        hello_time: DEFAULT_HELLO_TIME,
        max_age: timers.max_age,
        forward_delay: timers.forward_delay,
        hold_count: timers.hold_count,
        max_hops,
        region_name,
        region_revision,
        cist,
        mstis,
        ports: features,
    })
}

struct Timers {
    max_age: u8,
    forward_delay: u8,
    hold_count: u8,
}

fn timers(config: Option<&TimerConfig>, what: &str, err: &mut impl FnMut(String)) -> Timers {
    let c = config;
    if let Some(h) = c.and_then(|c| c.hello_time) {
        if h != DEFAULT_HELLO_TIME {
            err(format!(
                "{what}: hello-time {h} is not supported, only {DEFAULT_HELLO_TIME}"
            ));
        }
    }
    let t = Timers {
        max_age: c.and_then(|c| c.max_age).unwrap_or(DEFAULT_MAX_AGE),
        forward_delay: c
            .and_then(|c| c.forwarding_delay)
            .unwrap_or(DEFAULT_FORWARD_DELAY),
        hold_count: c.and_then(|c| c.hold_count).unwrap_or(DEFAULT_HOLD_COUNT),
    };
    // IEEE 802.1D 17.14: 2 * (Forward Delay - 1) >= Max Age.
    if 2 * (u16::from(t.forward_delay).saturating_sub(1)) < u16::from(t.max_age) {
        err(format!(
            "{what}: max-age {} is greater than 2 * (forwarding-delay {} - 1)",
            t.max_age, t.forward_delay
        ));
    }
    t
}

fn bridge_priority(priority: Option<u16>, what: &str, err: &mut impl FnMut(String)) -> u16 {
    match priority {
        None => DEFAULT_BRIDGE_PRIORITY,
        Some(p) if p % 4096 == 0 && p <= 61440 => p,
        Some(p) => {
            err(format!(
                "{what}: bridge-priority {p} is not a multiple of 4096 in 0..61440"
            ));
            DEFAULT_BRIDGE_PRIORITY
        }
    }
}

/// A spanning tree's bridge priority and ports: every switch port, with the
/// settings of its `interfaces` entry.
fn tree(
    priority: Option<u16>,
    interfaces: Option<&TreeInterfaces>,
    ports: &BTreeSet<String>,
    what: &str,
    err: &mut impl FnMut(String),
) -> Tree {
    let mut tree = Tree {
        bridge_priority: bridge_priority(priority, what, err),
        ports: ports
            .iter()
            .map(|p| (p.clone(), TreePort::default()))
            .collect(),
    };
    for interface in interfaces.map_or(&[][..], |i| &i.interface) {
        let Some(port) = tree.ports.get_mut(&interface.name) else {
            err(format!(
                "{what}: interface {} is not a configured switch port",
                interface.name
            ));
            continue;
        };
        port.cost = interface.config.cost;
        match interface.config.port_priority {
            None => {}
            Some(p) if p % 16 == 0 => port.priority = p,
            Some(p) => err(format!(
                "{what}: interface {}: port-priority {p} is not a multiple of 16",
                interface.name
            )),
        }
    }
    tree
}

fn mstis(
    instances: Option<&MstInstances>,
    ports: &BTreeSet<String>,
    err: &mut impl FnMut(String),
) -> BTreeMap<u16, Msti> {
    let mut mstis = BTreeMap::new();
    let mut msti_of_vlan: BTreeMap<u16, u16> = BTreeMap::new();
    for instance in instances.map_or(&[][..], |i| &i.mst_instance) {
        let Some(id) = instance.config.mst_id.or(instance.mst_id) else {
            err("mstp: mst-instance without mst-id".into());
            continue;
        };
        let what = format!("mstp mst-instance {id}");
        let mut vlans = BTreeSet::new();
        for entry in &instance.config.vlan {
            match vlan_entry(entry) {
                Ok(ids) => vlans.extend(ids),
                Err(e) => err(format!("{what}: {e}")),
            }
        }
        for vlan in &vlans {
            if let Some(other) = msti_of_vlan.insert(*vlan, id) {
                err(format!(
                    "{what}: VLAN {vlan} is already mapped to mst-instance {other}"
                ));
            }
        }
        let tree = tree(
            instance.config.bridge_priority,
            instance.interfaces.as_ref(),
            ports,
            &what,
            err,
        );
        mstis.insert(id, Msti { vlans, tree });
    }
    mstis
}

/// One `vlan` entry of an MSTI: a VLAN id, or a range "x..y" with all VLANs
/// in it.
fn vlan_entry(entry: &Scalar) -> Result<Vec<u16>, String> {
    let check = |id: u64| match u16::try_from(id) {
        Ok(v @ 1..=4094) => Ok(v),
        _ => Err(format!("VLAN id {id} is out of range 1..4094")),
    };
    if let Some(id) = entry.as_u64() {
        return check(id).map(|id| vec![id]);
    }
    let range = entry.to_string();
    let bounds = range.split_once("..").and_then(|(low, high)| {
        Some((
            low.trim().parse::<u64>().ok()?,
            high.trim().parse::<u64>().ok()?,
        ))
    });
    let Some((low, high)) = bounds else {
        return Err(format!(
            "vlan \"{range}\" is neither a VLAN id nor a range x..y"
        ));
    };
    let (low, high) = (check(low)?, check(high)?);
    if low >= high {
        return Err(format!("vlan range {range}: {low} is not below {high}"));
    }
    Ok((low..=high).collect())
}

/// Features of every switch port: the global defaults, overridden by
/// `/stp/interfaces`.
fn port_features(
    config: &StpConfig,
    global: Option<&GlobalConfig>,
    ports: &BTreeSet<String>,
    err: &mut impl FnMut(String),
) -> BTreeMap<String, PortFeatures> {
    let default = PortFeatures {
        bpdu_guard: global.and_then(|g| g.bpdu_guard).unwrap_or(false),
        bpdu_filter: global.and_then(|g| g.bpdu_filter).unwrap_or(false),
        ..PortFeatures::default()
    };
    let mut features: BTreeMap<String, PortFeatures> =
        ports.iter().map(|p| (p.clone(), default)).collect();

    let entries = config.interfaces.as_ref().map_or(&[][..], |i| &i.interface);
    for interface in entries {
        let name = &interface.name;
        let Some(f) = features.get_mut(name) else {
            err(format!(
                "interfaces: interface {name} is not a configured switch port"
            ));
            continue;
        };
        let c = &interface.config;
        match c.edge_port.as_deref().map(unprefixed) {
            None | Some("EDGE_AUTO") => f.edge = EdgePort::Auto,
            Some("EDGE_ENABLE") => f.edge = EdgePort::Enable,
            Some("EDGE_DISABLE") => f.edge = EdgePort::Disable,
            Some(other) => err(format!(
                "interfaces: interface {name}: edge-port {other} is not supported"
            )),
        }
        match c.link_type.as_deref() {
            None => f.point_to_point = None,
            Some("P2P") => f.point_to_point = Some(true),
            Some("SHARED") => f.point_to_point = Some(false),
            Some(other) => err(format!(
                "interfaces: interface {name}: link-type {other} is not supported"
            )),
        }
        match c.guard.as_deref() {
            None | Some("NONE") => f.root_guard = false,
            Some("ROOT") => f.root_guard = true,
            Some(other) => err(format!(
                "interfaces: interface {name}: guard {other} is not supported, only ROOT and NONE"
            )),
        }
        if let Some(b) = c.bpdu_guard {
            f.bpdu_guard = b;
        }
        if let Some(b) = c.bpdu_filter {
            f.bpdu_filter = b;
        }
    }
    features
}

// ---------------------------------------------------------------------------
// Operational state
// ---------------------------------------------------------------------------

/// State of spanning tree as mstpd reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StpState {
    /// The CIST in MSTP, the only tree otherwise.
    pub cist: TreeState,
    /// By MSTID.
    pub mstis: BTreeMap<u16, TreeState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeState {
    pub bridge: Option<BridgeState>,
    pub ports: BTreeMap<String, PortState>,
}

/// A bridge identifier: priority (without the system ID extension) and
/// MAC address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeId {
    pub priority: u16,
    /// "aa:bb:cc:dd:ee:ff".
    pub address: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeState {
    pub bridge: Option<BridgeId>,
    /// The root bridge: the CIST root, or the regional root of an MSTI.
    pub root: Option<BridgeId>,
    /// Interface name of the root port; None on the root bridge.
    pub root_port: Option<String>,
    pub root_cost: Option<u32>,
    pub topology_changes: Option<u64>,
    /// Seconds.
    pub time_since_topology_change: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortState {
    pub port_num: Option<u16>,
    /// ROOT, DESIGNATED, ALTERNATE or BACKUP.
    pub role: Option<&'static str>,
    /// DISABLED, BLOCKING, LEARNING or FORWARDING.
    pub port_state: Option<&'static str>,
    pub designated_root: Option<BridgeId>,
    pub designated_cost: Option<u32>,
    pub designated_bridge: Option<BridgeId>,
    pub designated_port_priority: Option<u8>,
    pub designated_port_num: Option<u16>,
    pub forward_transitions: Option<u64>,
    pub bpdu_sent: Option<u64>,
    pub bpdu_received: Option<u64>,
    /// The port's path cost in use, configured or from the link speed.
    pub path_cost: Option<u32>,
    pub oper_edge: Option<bool>,
    pub oper_point_to_point: Option<bool>,
}

/// State data of `/stp`: the applied configuration, and what mstpd reports
/// (None if mstpd does not answer).
pub(crate) fn stp_state_xml(xml: &mut String, stp: &Stp, state: Option<&StpState>) {
    let _ = write!(
        xml,
        r#"<stp xmlns="{STP_NS}"><global><state><enabled-protocol xmlns:sw="{SWITCH_NS}" {TYPES_XMLNS}>{}</enabled-protocol>"#,
        stp.protocol.identity()
    );
    xml.push_str("</state></global>");

    let empty = TreeState::default();
    let cist_state = state.map_or(&empty, |s| &s.cist);
    let timers = format!(
        "<hello-time>{}</hello-time><max-age>{}</max-age><forwarding-delay>{}</forwarding-delay><hold-count>{}</hold-count>",
        stp.hello_time, stp.max_age, stp.forward_delay, stp.hold_count
    );
    match stp.protocol {
        StpProtocol::Stp | StpProtocol::Rstp => {
            let _ = write!(
                xml,
                "<rstp><state>{timers}<bridge-priority>{}</bridge-priority>",
                stp.cist.bridge_priority
            );
            common_state_xml(xml, cist_state.bridge.as_ref());
            xml.push_str("</state>");
            tree_ports_xml(xml, "interfaces", &stp.cist, cist_state);
            xml.push_str("</rstp>");
        }
        StpProtocol::Mstp => {
            xml.push_str("<mstp><state>");
            if let Some(name) = &stp.region_name {
                let _ = write!(xml, "<name>{}</name>", escape(name));
            }
            let _ = write!(
                xml,
                r#"<revision>{}</revision><max-hop>{}</max-hop>{timers}<bridge-priority xmlns="{SWITCH_NS}">{}</bridge-priority>"#,
                stp.region_revision, stp.max_hops, stp.cist.bridge_priority
            );
            // The CIST's state leaves are clixon-switch augments.
            let mut common = String::new();
            common_state_xml(&mut common, cist_state.bridge.as_ref());
            xml.push_str(&namespaced(&common, SWITCH_NS));
            xml.push_str("</state><mst-instances>");
            for (id, msti) in &stp.mstis {
                let msti_state = state.and_then(|s| s.mstis.get(id)).unwrap_or(&empty);
                let _ = write!(
                    xml,
                    "<mst-instance><mst-id>{id}</mst-id><state><mst-id>{id}</mst-id>"
                );
                for range in vlan_ranges(&msti.vlans) {
                    let _ = write!(xml, "<vlan>{range}</vlan>");
                }
                let _ = write!(
                    xml,
                    "<bridge-priority>{}</bridge-priority>",
                    msti.tree.bridge_priority
                );
                common_state_xml(xml, msti_state.bridge.as_ref());
                xml.push_str("</state>");
                tree_ports_xml(xml, "interfaces", &msti.tree, msti_state);
                xml.push_str("</mst-instance>");
            }
            xml.push_str("</mst-instances>");
            let mut cist_ports = String::new();
            tree_ports_xml(&mut cist_ports, "interfaces", &stp.cist, cist_state);
            xml.push_str(&cist_ports.replacen(
                "<interfaces>",
                &format!(r#"<interfaces xmlns="{SWITCH_NS}">"#),
                1,
            ));
            xml.push_str("</mstp>");
        }
    }

    xml.push_str("<interfaces>");
    for (name, f) in &stp.ports {
        let _ = write!(
            xml,
            "<interface><name>{n}</name><state><name>{n}</name><edge-port {TYPES_XMLNS}>{}</edge-port>",
            f.edge.identity(),
            n = escape(name),
        );
        if let Some(p2p) = f.point_to_point {
            let _ = write!(
                xml,
                "<link-type>{}</link-type>",
                if p2p { "P2P" } else { "SHARED" }
            );
        }
        let _ = write!(
            xml,
            "<guard>{}</guard><bpdu-guard>{}</bpdu-guard><bpdu-filter>{}</bpdu-filter></state></interface>",
            if f.root_guard { "ROOT" } else { "NONE" },
            f.bpdu_guard,
            f.bpdu_filter
        );
    }
    xml.push_str("</interfaces></stp>");
}

/// Puts the top-level elements of `fragment`, a sequence of leaves as
/// [`common_state_xml`] writes them, into namespace `ns`.
fn namespaced(fragment: &str, ns: &str) -> String {
    let mut out = String::new();
    for (i, part) in fragment.split('<').enumerate() {
        if i > 0 {
            out.push('<');
        }
        match part.find('>') {
            Some(end) if !part.starts_with('/') => {
                let _ = write!(out, r#"{} xmlns="{ns}"{}"#, &part[..end], &part[end..]);
            }
            _ => out.push_str(part),
        }
    }
    out
}

/// stp-common-state leaves.
fn common_state_xml(xml: &mut String, bridge: Option<&BridgeState>) {
    let Some(b) = bridge else {
        return;
    };
    if let Some(id) = &b.bridge {
        let _ = write!(xml, "<bridge-address>{}</bridge-address>", id.address);
    }
    if let Some(root) = &b.root {
        let _ = write!(
            xml,
            "<designated-root-priority>{}</designated-root-priority><designated-root-address>{}</designated-root-address>",
            root.priority, root.address
        );
    }
    if let Some(port) = &b.root_port {
        let _ = write!(xml, "<root-port>{}</root-port>", escape(port));
    }
    if let Some(cost) = b.root_cost {
        let _ = write!(xml, "<root-cost>{cost}</root-cost>");
    }
    if let Some(n) = b.topology_changes {
        let _ = write!(xml, "<topology-changes>{n}</topology-changes>");
    }
}

/// stp-interfaces-top state of one tree.
fn tree_ports_xml(xml: &mut String, element: &str, tree: &Tree, state: &TreeState) {
    let _ = write!(xml, "<{element}>");
    for (name, port) in &tree.ports {
        let _ = write!(
            xml,
            "<interface><name>{n}</name><state><name>{n}</name>",
            n = escape(name)
        );
        if let Some(cost) = port.cost {
            let _ = write!(xml, "<cost>{cost}</cost>");
        }
        let _ = write!(xml, "<port-priority>{}</port-priority>", port.priority);
        if let Some(s) = state.ports.get(name) {
            port_state_xml(xml, s);
        }
        xml.push_str("</state></interface>");
    }
    let _ = write!(xml, "</{element}>");
}

fn port_state_xml(xml: &mut String, s: &PortState) {
    if let Some(n) = s.port_num {
        let _ = write!(xml, "<port-num>{n}</port-num>");
    }
    if let Some(role) = s.role {
        let _ = write!(xml, "<role {TYPES_XMLNS}>oc-stp-types:{role}</role>");
    }
    if let Some(state) = s.port_state {
        let _ = write!(
            xml,
            "<port-state {TYPES_XMLNS}>oc-stp-types:{state}</port-state>"
        );
    }
    if let Some(root) = &s.designated_root {
        let _ = write!(
            xml,
            "<designated-root-priority>{}</designated-root-priority><designated-root-address>{}</designated-root-address>",
            root.priority, root.address
        );
    }
    if let Some(cost) = s.designated_cost {
        let _ = write!(xml, "<designated-cost>{cost}</designated-cost>");
    }
    if let Some(bridge) = &s.designated_bridge {
        let _ = write!(
            xml,
            "<designated-bridge-priority>{}</designated-bridge-priority><designated-bridge-address>{}</designated-bridge-address>",
            bridge.priority, bridge.address
        );
    }
    if let Some(p) = s.designated_port_priority {
        let _ = write!(
            xml,
            "<designated-port-priority>{p}</designated-port-priority>"
        );
    }
    if let Some(n) = s.designated_port_num {
        let _ = write!(xml, "<designated-port-num>{n}</designated-port-num>");
    }
    if let Some(n) = s.forward_transitions {
        // Sic: the leaf is misspelt in openconfig-spanning-tree.
        let _ = write!(xml, "<forward-transisitions>{n}</forward-transisitions>");
    }
    if s.bpdu_sent.is_some() || s.bpdu_received.is_some() {
        xml.push_str("<counters>");
        if let Some(n) = s.bpdu_sent {
            let _ = write!(xml, "<bpdu-sent>{n}</bpdu-sent>");
        }
        if let Some(n) = s.bpdu_received {
            let _ = write!(xml, "<bpdu-received>{n}</bpdu-received>");
        }
        xml.push_str("</counters>");
    }
}

/// `vlans` as ids and ranges "x..y".
pub fn vlan_ranges(vlans: &BTreeSet<u16>) -> Vec<String> {
    let mut ranges: Vec<(u16, u16)> = Vec::new();
    for vlan in vlans {
        match ranges.last_mut() {
            Some((_, high)) if *high + 1 == *vlan => *high = *vlan,
            _ => ranges.push((*vlan, *vlan)),
        }
    }
    ranges
        .into_iter()
        .map(|(low, high)| match low == high {
            true => low.to_string(),
            false => format!("{low}..{high}"),
        })
        .collect()
}
