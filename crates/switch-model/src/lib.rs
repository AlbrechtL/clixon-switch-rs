//! The part of the OpenConfig model that the switch implements, and its
//! translation into a validated [`DesiredState`].
//!
//! The input is the RFC 7951 JSON that clixon prints for a datastore tree
//! (`xml2json_cbuf_vec` with `skiptop`). Only what the plugin acts on is
//! modelled; serde skips everything else. clixon has already validated the
//! tree against YANG (types, ranges, mandatory leaves, deviations), so the
//! checks here are the ones YANG cannot express: that a port exists on this
//! switch, that a VLAN is declared, one routed VLAN interface per VLAN, and
//! so on.

mod stp;
mod supported;
mod system;

use serde::{de, Deserialize, Deserializer};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::Ipv4Addr;

pub use system::{
    date_and_time, parse_loadavg, parse_meminfo, parse_os_release, parse_uptime, system_state_xml,
    SystemState,
};

pub use stp::{
    vlan_ranges, BridgeId, BridgeState, EdgePort, Msti, PortFeatures, PortState, Stp, StpProtocol,
    StpState, Tree, TreePort, TreeState, DEFAULT_BRIDGE_PRIORITY, DEFAULT_FORWARD_DELAY,
    DEFAULT_HELLO_TIME, DEFAULT_HOLD_COUNT, DEFAULT_MAX_AGE, DEFAULT_MAX_HOPS,
    DEFAULT_PORT_PRIORITY,
};

/// Linux bridge that holds all switch ports. It is an implementation detail,
/// not part of the model, so no interface may be configured with this name.
pub const BRIDGE_NAME: &str = "br-lan";

/// IFNAMSIZ - 1.
const MAX_IFNAME_LEN: usize = 15;

/// Namespace of the clixon-switch YANG module.
const SWITCH_NS: &str = "urn:github:albrechtl:clixon-switch";

// ---------------------------------------------------------------------------
// RFC 7951 JSON
// ---------------------------------------------------------------------------

/// Top of a datastore tree.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(rename = "openconfig-interfaces:interfaces")]
    pub interfaces: Option<Interfaces>,
    #[serde(rename = "clixon-switch:vlans")]
    pub vlans: Option<Vlans>,
    #[serde(rename = "clixon-switch:switch")]
    pub switch: Option<Switch>,
    #[serde(rename = "clixon-switch:port-based-vlans")]
    pub port_based_vlans: Option<PortBasedVlans>,
    #[serde(rename = "openconfig-spanning-tree:stp")]
    pub stp: Option<stp::StpConfig>,
}

impl Config {
    /// Parses clixon's JSON. An empty datastore prints as an empty string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        if json.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(json)
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Interfaces {
    #[serde(default)]
    pub interface: Vec<Interface>,
}

#[derive(Debug, Deserialize)]
pub struct Interface {
    pub name: String,
    #[serde(default)]
    pub config: InterfaceConfig,
    #[serde(rename = "openconfig-if-ethernet:ethernet")]
    pub ethernet: Option<Ethernet>,
    #[serde(rename = "openconfig-vlan:routed-vlan")]
    pub routed_vlan: Option<RoutedVlan>,
}

#[derive(Debug, Default, Deserialize)]
pub struct InterfaceConfig {
    /// identityref, e.g. "iana-if-type:ethernetCsmacd".
    #[serde(rename = "type")]
    pub if_type: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Ethernet {
    #[serde(rename = "openconfig-vlan:switched-vlan")]
    pub switched_vlan: Option<SwitchedVlan>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SwitchedVlan {
    #[serde(default)]
    pub config: SwitchedVlanConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SwitchedVlanConfig {
    pub interface_mode: Option<String>,
    #[serde(default, deserialize_with = "opt_int")]
    pub access_vlan: Option<u16>,
    #[serde(default, deserialize_with = "opt_int")]
    pub native_vlan: Option<u16>,
    /// VLAN ids, or ranges "x..y".
    #[serde(default)]
    pub trunk_vlans: Vec<Scalar>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RoutedVlan {
    #[serde(default)]
    pub config: RoutedVlanConfig,
    #[serde(rename = "openconfig-if-ip:ipv4")]
    pub ipv4: Option<Ipv4>,
}

#[derive(Debug, Default, Deserialize)]
pub struct RoutedVlanConfig {
    /// union { uint16; string }: a VLAN id or a VLAN name.
    pub vlan: Option<Scalar>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Ipv4 {
    #[serde(default)]
    pub addresses: Addresses,
    #[serde(default)]
    pub config: Ipv4Config,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Ipv4Config {
    pub dhcp_client: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Addresses {
    #[serde(default)]
    pub address: Vec<Address>,
}

#[derive(Debug, Deserialize)]
pub struct Address {
    pub ip: String,
    #[serde(default)]
    pub config: AddressConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AddressConfig {
    #[serde(default, deserialize_with = "opt_int")]
    pub prefix_length: Option<u8>,
}

/// `/vlans`: the VLAN database.
#[derive(Debug, Default, Deserialize)]
pub struct Vlans {
    #[serde(default)]
    pub vlan: Vec<VlanEntry>,
}

#[derive(Debug, Deserialize)]
pub struct VlanEntry {
    #[serde(rename = "vlan-id", default, deserialize_with = "opt_int")]
    pub vlan_id: Option<u16>,
    #[serde(default)]
    pub config: VlanEntryConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct VlanEntryConfig {
    #[serde(default, deserialize_with = "opt_int")]
    pub vlan_id: Option<u16>,
    pub name: Option<String>,
    /// ACTIVE (default) or SUSPENDED.
    pub status: Option<String>,
}

/// `/switch`: switch-wide settings.
#[derive(Debug, Default, Deserialize)]
pub struct Switch {
    #[serde(default)]
    pub config: SwitchConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SwitchConfig {
    /// DOT1Q (default) or PORT_BASED.
    pub vlan_mode: Option<String>,
}

/// `/port-based-vlans`.
#[derive(Debug, Default, Deserialize)]
pub struct PortBasedVlans {
    #[serde(default)]
    pub group: Vec<Group>,
}

#[derive(Debug, Deserialize)]
pub struct Group {
    #[serde(default, deserialize_with = "opt_int")]
    pub id: Option<u16>,
    #[serde(default)]
    pub config: GroupConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct GroupConfig {
    #[serde(default, deserialize_with = "opt_int")]
    pub id: Option<u16>,
    pub name: Option<String>,
    #[serde(default)]
    pub port: Vec<String>,
}

/// A leaf that may arrive as a JSON number or a string. RFC 7951 quotes
/// only 64-bit integers, but numeric strings are accepted as well.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    Number(u64),
    String(String),
}

impl Scalar {
    pub(crate) fn as_u64(&self) -> Option<u64> {
        match self {
            Scalar::Number(n) => Some(*n),
            Scalar::String(s) => s.parse().ok(),
        }
    }
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scalar::Number(n) => write!(f, "{n}"),
            Scalar::String(s) => f.write_str(s),
        }
    }
}

fn opt_int<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: TryFrom<u64>,
{
    Option::<Scalar>::deserialize(deserializer)?
        .map(|s| {
            s.as_u64()
                .and_then(|n| T::try_from(n).ok())
                .ok_or_else(|| de::Error::custom(format!("integer out of range: {s}")))
        })
        .transpose()
}

// ---------------------------------------------------------------------------
// Desired state
// ---------------------------------------------------------------------------

/// What the kernel should look like, independent of how it gets there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesiredState {
    pub mode: VlanMode,
    /// The VLANs by id: the declared VLANs in 802.1Q mode, the groups in
    /// port-based mode.
    pub vlans: BTreeMap<u16, Vlan>,
    /// Switch ports by name. Ports of the switch that are missing here are
    /// taken out of the bridge and set down.
    pub ports: BTreeMap<String, Port>,
    /// Routed VLAN interfaces by name.
    pub svis: BTreeMap<String, Svi>,
    /// Spanning tree, while a protocol is enabled.
    pub stp: Option<Stp>,
}

impl DesiredState {
    /// Whether frames of `vlan` are forwarded at all. VLANs missing from
    /// [`DesiredState::vlans`] count as active, so that a state built by hand
    /// needs no VLAN database.
    pub fn vlan_active(&self, vlan: u16) -> bool {
        self.vlans.get(&vlan).is_none_or(|v| v.active)
    }

    /// The SVI that runs the DHCP client, if any.
    pub fn dhcp_svi(&self) -> Option<&str> {
        self.svis
            .iter()
            .find(|(_, svi)| svi.dhcp_client)
            .map(|(name, _)| name.as_str())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VlanMode {
    /// IEEE 802.1Q: declared VLANs, access and trunk ports.
    #[default]
    Dot1q,
    /// Ports in disjoint, untagged groups.
    PortBased,
}

impl VlanMode {
    fn as_str(self) -> &'static str {
        match self {
            VlanMode::Dot1q => "DOT1Q",
            VlanMode::PortBased => "PORT_BASED",
        }
    }
}

/// A declared VLAN, or a port-based VLAN group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vlan {
    pub name: Option<String>,
    /// false for status SUSPENDED: no port carries the VLAN.
    pub active: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Port {
    pub enabled: bool,
    /// Untagged VLAN and PVID of the port: the access VLAN, the native VLAN
    /// of a trunk, or the port's group. None: untagged frames are dropped.
    pub native_vlan: Option<u16>,
    /// VLANs the port carries tagged. Never contains `native_vlan`.
    pub tagged_vlans: BTreeSet<u16>,
}

impl Port {
    /// An access port, or a port in a port-based group.
    pub fn access(vlan: u16) -> Self {
        Port {
            enabled: true,
            native_vlan: Some(vlan),
            tagged_vlans: BTreeSet::new(),
        }
    }

    /// Whether the port carries `vlan`, tagged or not.
    pub fn carries(&self, vlan: u16) -> bool {
        self.native_vlan == Some(vlan) || self.tagged_vlans.contains(&vlan)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svi {
    pub enabled: bool,
    pub vlan: u16,
    /// Static addresses.
    pub addresses: BTreeSet<Ipv4Prefix>,
    /// Whether a DHCP client runs on the interface. At most one SVI has it.
    pub dhcp_client: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ipv4Prefix {
    pub addr: Ipv4Addr,
    pub prefix_len: u8,
}

impl fmt::Display for Ipv4Prefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix_len)
    }
}

/// A configuration the switch cannot apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// The interface the error is about, empty for switch-wide errors.
    pub interface: String,
    pub message: String,
}

impl Error {
    fn global(message: String) -> Self {
        Error {
            interface: String::new(),
            message,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.interface.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "interface {}: {}", self.interface, self.message)
        }
    }
}

/// All problems found in one configuration, so that one commit attempt
/// reports every one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Errors(pub Vec<Error>);

impl fmt::Display for Errors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Errors {}

/// Parses clixon's JSON for a datastore tree, rejects configuration the
/// switch does not implement, and translates the rest into a
/// [`DesiredState`]. Reports all problems at once.
///
/// `switch_ports` are the port names that exist on this switch.
pub fn validate(json: &str, switch_ports: &BTreeSet<String>) -> Result<DesiredState, Errors> {
    let parse_error =
        |e: serde_json::Error| Error::global(format!("cannot parse the configuration: {e}"));
    let value: serde_json::Value = if json.trim().is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(json).map_err(|e| Errors(vec![parse_error(e)]))?
    };

    let mut errors = supported::check(&value);
    match serde_json::from_value::<Config>(value) {
        Ok(config) => match desired_state(&config, switch_ports) {
            Ok(state) if errors.is_empty() => return Ok(state),
            Ok(_) => {}
            Err(Errors(e)) => errors.extend(e),
        },
        Err(e) => errors.push(parse_error(e)),
    }
    Err(Errors(errors))
}

/// Validates `config` and translates it into a [`DesiredState`].
///
/// `switch_ports` are the port names that exist on this switch, e.g. the DSA
/// user ports lan1..lan8.
pub fn desired_state(
    config: &Config,
    switch_ports: &BTreeSet<String>,
) -> Result<DesiredState, Errors> {
    let mut errors = Vec::new();
    let interfaces = config.interfaces.as_ref().map_or(&[][..], |i| &i.interface);

    let mode = match config
        .switch
        .as_ref()
        .and_then(|s| s.config.vlan_mode.as_deref())
    {
        None | Some("DOT1Q") => VlanMode::Dot1q,
        Some("PORT_BASED") => VlanMode::PortBased,
        Some(other) => {
            errors.push(Error::global(format!(
                "switch vlan-mode {other} is not supported"
            )));
            VlanMode::Dot1q
        }
    };
    let domain = match mode {
        VlanMode::Dot1q => dot1q_vlans(config, &mut errors),
        VlanMode::PortBased => port_groups(config, interfaces, switch_ports, &mut errors),
    };

    let mut state = DesiredState {
        mode,
        ..DesiredState::default()
    };
    for interface in interfaces {
        let mut err = |message: String| {
            errors.push(Error {
                interface: interface.name.clone(),
                message,
            })
        };

        // Compare the identity without its prefix: RFC 7951 qualifies it with
        // the module name, XML-derived trees may carry the YANG prefix.
        let if_type = interface.config.if_type.as_deref();
        match if_type.map(unprefixed) {
            Some("ethernetCsmacd") => match port(interface, switch_ports, &domain) {
                Ok(p) => {
                    state.ports.insert(interface.name.clone(), p);
                }
                Err(messages) => messages.into_iter().for_each(&mut err),
            },
            Some("l3ipvlan") => match svi(interface, switch_ports, &domain) {
                Ok(s) => {
                    state.svis.insert(interface.name.clone(), s);
                }
                Err(messages) => messages.into_iter().for_each(&mut err),
            },
            Some(_) => err(format!(
                "type {} is not supported, only ethernetCsmacd (switch ports) and l3ipvlan (routed VLANs)",
                if_type.unwrap_or_default()
            )),
            None => err("type is missing".into()),
        }
    }
    state.vlans = domain.vlans;
    let ports = state.ports.keys().cloned().collect();
    state.stp = stp::desired_stp(config.stp.as_ref(), &ports, &mut errors);

    check_unique_vlans(&state, &mut errors);
    check_unique_addresses(&state, &mut errors);
    check_single_dhcp_client(&state, &mut errors);

    if errors.is_empty() {
        Ok(state)
    } else {
        Err(Errors(errors))
    }
}

fn unprefixed(identity: &str) -> &str {
    identity.rsplit(':').next().unwrap_or(identity)
}

/// The VLANs ports and SVIs may use, in either mode.
struct Domain {
    mode: VlanMode,
    vlans: BTreeMap<u16, Vlan>,
    /// Port-based mode: the group of each port.
    group_of_port: BTreeMap<String, u16>,
}

impl Domain {
    /// Checks that `id` is a valid, declared VLAN (or group).
    fn declared(&self, id: u64) -> Result<u16, String> {
        let id = check_vlan_id(id)?;
        match (self.vlans.contains_key(&id), self.mode) {
            (true, _) => Ok(id),
            (false, VlanMode::Dot1q) => Err(format!("VLAN {id} is not declared in vlans")),
            (false, VlanMode::PortBased) => {
                Err(format!("port-based-vlans group {id} does not exist"))
            }
        }
    }

    fn active(&self, id: u16) -> bool {
        self.vlans.get(&id).is_some_and(|v| v.active)
    }
}

fn dot1q_vlans(config: &Config, errors: &mut Vec<Error>) -> Domain {
    if config
        .port_based_vlans
        .as_ref()
        .is_some_and(|p| !p.group.is_empty())
    {
        errors.push(Error::global(
            "port-based-vlans requires switch vlan-mode PORT_BASED".into(),
        ));
    }

    let mut vlans = BTreeMap::new();
    for entry in config.vlans.as_ref().map_or(&[][..], |v| &v.vlan) {
        let Some(id) = entry.config.vlan_id.or(entry.vlan_id) else {
            errors.push(Error::global("vlans: vlan without vlan-id".into()));
            continue;
        };
        let id = match check_vlan_id(u64::from(id)) {
            Ok(id) => id,
            Err(e) => {
                errors.push(Error::global(format!("vlans: {e}")));
                continue;
            }
        };
        let active = match entry.config.status.as_deref() {
            None | Some("ACTIVE") => true,
            Some("SUSPENDED") => false,
            Some(other) => {
                errors.push(Error::global(format!(
                    "vlans: VLAN {id}: status {other} is not supported"
                )));
                true
            }
        };
        vlans.insert(
            id,
            Vlan {
                name: entry.config.name.clone(),
                active,
            },
        );
    }

    Domain {
        mode: VlanMode::Dot1q,
        vlans,
        group_of_port: BTreeMap::new(),
    }
}

fn port_groups(
    config: &Config,
    interfaces: &[Interface],
    switch_ports: &BTreeSet<String>,
    errors: &mut Vec<Error>,
) -> Domain {
    if config.vlans.as_ref().is_some_and(|v| !v.vlan.is_empty()) {
        errors.push(Error::global(
            "vlans requires switch vlan-mode DOT1Q; in PORT_BASED mode use port-based-vlans".into(),
        ));
    }

    let mut vlans = BTreeMap::new();
    let mut group_of_port = BTreeMap::new();
    for group in config
        .port_based_vlans
        .as_ref()
        .map_or(&[][..], |p| &p.group)
    {
        let Some(id) = group.config.id.or(group.id) else {
            errors.push(Error::global("port-based-vlans: group without id".into()));
            continue;
        };
        let id = match check_vlan_id(u64::from(id)) {
            Ok(id) => id,
            Err(e) => {
                errors.push(Error::global(format!("port-based-vlans group {id}: {e}")));
                continue;
            }
        };
        vlans.insert(
            id,
            Vlan {
                name: group.config.name.clone(),
                active: true,
            },
        );

        for port in &group.config.port {
            let is_port = interfaces.iter().any(|i| {
                &i.name == port
                    && i.config.if_type.as_deref().map(unprefixed) == Some("ethernetCsmacd")
            });
            if !is_port || !switch_ports.contains(port) {
                errors.push(Error::global(format!(
                    "port-based-vlans group {id}: {port} is not a configured switch port"
                )));
            } else if let Some(other) = group_of_port.insert(port.clone(), id) {
                errors.push(Error::global(format!(
                    "port-based-vlans group {id}: {port} is already a member of group {other}"
                )));
            }
        }
    }

    Domain {
        mode: VlanMode::PortBased,
        vlans,
        group_of_port,
    }
}

fn port(
    interface: &Interface,
    switch_ports: &BTreeSet<String>,
    domain: &Domain,
) -> Result<Port, Vec<String>> {
    let mut errors = Vec::new();

    if !switch_ports.contains(&interface.name) {
        let available: Vec<_> = switch_ports.iter().map(String::as_str).collect();
        errors.push(format!(
            "not a port of this switch (ports: {})",
            available.join(", ")
        ));
    }
    if interface.routed_vlan.is_some() {
        errors.push("routed-vlan is only supported on l3ipvlan interfaces".into());
    }

    let vlan_config = interface
        .ethernet
        .as_ref()
        .and_then(|e| e.switched_vlan.as_ref())
        .map(|s| &s.config);

    let vlans = match domain.mode {
        VlanMode::Dot1q => match vlan_config {
            None => {
                errors.push("ethernet/switched-vlan/config is required: interface-mode ACCESS with an access-vlan, or TRUNK".into());
                None
            }
            Some(c) => switched_vlans(c, domain, &mut errors),
        },
        VlanMode::PortBased => {
            if vlan_config.is_some() {
                errors.push("switched-vlan requires switch vlan-mode DOT1Q; in PORT_BASED mode add the port to a port-based-vlans group".into());
            }
            match domain.group_of_port.get(&interface.name) {
                Some(group) => Some((Some(*group), BTreeSet::new())),
                None => {
                    errors.push("not a member of any port-based-vlans group".into());
                    None
                }
            }
        }
    };

    match vlans {
        Some((native_vlan, tagged_vlans)) if errors.is_empty() => Ok(Port {
            enabled: interface.config.enabled.unwrap_or(true),
            native_vlan,
            tagged_vlans,
        }),
        _ => Err(errors),
    }
}

/// The native and tagged VLANs of an 802.1Q port. Suspended VLANs are left
/// out.
fn switched_vlans(
    c: &SwitchedVlanConfig,
    domain: &Domain,
    errors: &mut Vec<String>,
) -> Option<(Option<u16>, BTreeSet<u16>)> {
    match c.interface_mode.as_deref() {
        Some("ACCESS") | None => {
            // YANG's when statements should keep these out, but clixon 7.8
            // does not enforce them on every path.
            let trunk_leaves = c.native_vlan.is_some() || !c.trunk_vlans.is_empty();
            let access = c.access_vlan.map(|v| declared(domain, v, errors));
            if trunk_leaves {
                errors.push("native-vlan and trunk-vlans require interface-mode TRUNK".into());
            }
            match access {
                None => {
                    errors.push("switched-vlan access-vlan is required".into());
                    None
                }
                Some(None) => None,
                Some(Some(vlan)) => {
                    Some((Some(vlan).filter(|v| domain.active(*v)), BTreeSet::new()))
                }
            }
        }
        Some("TRUNK") => {
            let native = c.native_vlan.map(|v| declared(domain, v, errors));
            let mut ok = !matches!(native, Some(None));
            if c.access_vlan.is_some() {
                errors.push("access-vlan requires interface-mode ACCESS".into());
                ok = false;
            }

            let mut tagged = BTreeSet::new();
            if c.trunk_vlans.is_empty() {
                // All VLANs allowed: every declared one.
                tagged.extend(domain.vlans.keys());
            }
            for entry in &c.trunk_vlans {
                match trunk_entry(entry, domain) {
                    Ok(ids) => tagged.extend(ids),
                    Err(e) => {
                        errors.push(e);
                        ok = false;
                    }
                }
            }

            let native = native.flatten();
            tagged.retain(|v| Some(*v) != native && domain.active(*v));
            ok.then(|| (native.filter(|v| domain.active(*v)), tagged))
        }
        Some(mode) => {
            errors.push(format!("interface-mode {mode} is not supported"));
            None
        }
    }
}

/// `id` if it is declared, otherwise None and an error.
fn declared(domain: &Domain, id: u16, errors: &mut Vec<String>) -> Option<u16> {
    domain
        .declared(u64::from(id))
        .map_err(|e| errors.push(e))
        .ok()
}

/// One trunk-vlans entry: a declared VLAN id, or a range "x..y" standing for
/// the declared VLANs in it.
fn trunk_entry(entry: &Scalar, domain: &Domain) -> Result<Vec<u16>, String> {
    if let Some(id) = entry.as_u64() {
        return domain.declared(id).map(|id| vec![id]);
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
            "trunk-vlans \"{range}\" is neither a VLAN id nor a range x..y"
        ));
    };
    let (low, high) = (check_vlan_id(low)?, check_vlan_id(high)?);
    if low >= high {
        return Err(format!(
            "trunk-vlans range {range}: {low} is not below {high}"
        ));
    }
    Ok(domain.vlans.range(low..=high).map(|(id, _)| *id).collect())
}

fn svi(
    interface: &Interface,
    switch_ports: &BTreeSet<String>,
    domain: &Domain,
) -> Result<Svi, Vec<String>> {
    let mut errors = Vec::new();
    let name = interface.name.as_str();

    if let Err(e) = check_ifname(name) {
        errors.push(e);
    }
    if name == BRIDGE_NAME || switch_ports.contains(name) {
        errors.push("name is reserved for a switch port or the bridge".into());
    }
    if interface
        .ethernet
        .as_ref()
        .is_some_and(|e| e.switched_vlan.is_some())
    {
        errors.push("switched-vlan is only supported on switch ports".into());
    }

    let Some(routed) = interface.routed_vlan.as_ref() else {
        errors.push("routed-vlan/config/vlan is required".into());
        return Err(errors);
    };

    let vlan = match routed.config.vlan.as_ref() {
        None => Err("routed-vlan/config/vlan is required".into()),
        Some(v) => match v.as_u64() {
            Some(id) => domain.declared(id),
            None => vlan_by_name(&v.to_string(), domain),
        },
    };
    let vlan = vlan.map_err(|e| errors.push(e)).ok();

    let mut addresses = BTreeSet::new();
    let dhcp_client = routed
        .ipv4
        .as_ref()
        .and_then(|i| i.config.dhcp_client)
        .unwrap_or(false);
    let entries = routed
        .ipv4
        .as_ref()
        .map_or(&[][..], |i| &i.addresses.address);
    for address in entries {
        let addr = match address.ip.parse::<Ipv4Addr>() {
            Ok(a) => a,
            Err(_) => {
                errors.push(format!("{} is not an IPv4 address", address.ip));
                continue;
            }
        };
        match address.config.prefix_length {
            Some(prefix_len @ 0..=32) => {
                addresses.insert(Ipv4Prefix { addr, prefix_len });
            }
            Some(p) => errors.push(format!("{addr}: prefix-length {p} is out of range")),
            None => errors.push(format!("{addr}: prefix-length is required")),
        }
    }

    match vlan {
        Some(vlan) if errors.is_empty() => Ok(Svi {
            enabled: interface.config.enabled.unwrap_or(true),
            vlan,
            addresses,
            dhcp_client,
        }),
        _ => Err(errors),
    }
}

fn vlan_by_name(name: &str, domain: &Domain) -> Result<u16, String> {
    let what = match domain.mode {
        VlanMode::Dot1q => "VLAN",
        VlanMode::PortBased => "port-based-vlans group",
    };
    let ids: Vec<u16> = domain
        .vlans
        .iter()
        .filter(|(_, v)| v.name.as_deref() == Some(name))
        .map(|(id, _)| *id)
        .collect();
    match ids[..] {
        [id] => Ok(id),
        [] => Err(format!(
            "routed-vlan vlan \"{name}\": no {what} has this name"
        )),
        _ => Err(format!(
            "routed-vlan vlan \"{name}\": the name is ambiguous ({what}s {})",
            ids.iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn check_vlan_id(id: u64) -> Result<u16, String> {
    match u16::try_from(id) {
        Ok(v @ 1..=4094) => Ok(v),
        _ => Err(format!("VLAN id {id} is out of range 1..4094")),
    }
}

/// Linux accepts any name of 1..15 bytes without '/', ':' or whitespace,
/// except "." and "..".
fn check_ifname(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name.len() <= MAX_IFNAME_LEN
        && name != "."
        && name != ".."
        && !name.contains(|c: char| c == '/' || c == ':' || c.is_whitespace());
    if valid {
        Ok(())
    } else {
        Err(format!(
            "\"{name}\" is not a valid Linux interface name (1..{MAX_IFNAME_LEN} bytes, no '/', ':' or spaces)"
        ))
    }
}

fn check_unique_vlans(state: &DesiredState, errors: &mut Vec<Error>) {
    let mut by_vlan: BTreeMap<u16, &str> = BTreeMap::new();
    for (name, svi) in &state.svis {
        if let Some(other) = by_vlan.insert(svi.vlan, name) {
            errors.push(Error {
                interface: name.clone(),
                message: format!("VLAN {} already has a routed interface, {other}", svi.vlan),
            });
        }
    }
}

fn check_unique_addresses(state: &DesiredState, errors: &mut Vec<Error>) {
    let mut by_addr: BTreeMap<Ipv4Addr, &str> = BTreeMap::new();
    for (name, svi) in &state.svis {
        for prefix in &svi.addresses {
            if let Some(other) = by_addr.insert(prefix.addr, name) {
                if other != name {
                    errors.push(Error {
                        interface: name.clone(),
                        message: format!(
                            "address {} is already configured on {other}",
                            prefix.addr
                        ),
                    });
                }
            }
        }
    }
}

/// One DHCP client at most: it owns the default route and resolv.conf.
fn check_single_dhcp_client(state: &DesiredState, errors: &mut Vec<Error>) {
    let mut names = state
        .svis
        .iter()
        .filter(|(_, svi)| svi.dhcp_client)
        .map(|(name, _)| name);
    if let Some(first) = names.next() {
        for name in names {
            errors.push(Error {
                interface: name.clone(),
                message: format!(
                    "ipv4 dhcp-client is already enabled on {first}; only one interface may run a DHCP client"
                ),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Operational state
// ---------------------------------------------------------------------------

/// Operational state of one interface, as the kernel reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceState {
    pub admin_up: bool,
    /// OpenConfig oper-status: UP, DOWN, LOWER_LAYER_DOWN, ...
    pub oper_status: &'static str,
    /// "aa:bb:cc:dd:ee:ff".
    pub mac: Option<String>,
    pub in_octets: u64,
    pub in_pkts: u64,
    pub out_octets: u64,
    pub out_pkts: u64,
    /// IPv4 addresses on the interface.
    pub addresses: BTreeMap<Ipv4Prefix, AddressOrigin>,
    /// The DHCP lease, while the interface's DHCP client holds one.
    pub dhcp_lease: Option<DhcpLease>,
}

/// OpenConfig ip-address-origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressOrigin {
    Static,
    Dhcp,
}

impl AddressOrigin {
    fn as_str(self) -> &'static str {
        match self {
            AddressOrigin::Static => "STATIC",
            AddressOrigin::Dhcp => "DHCP",
        }
    }
}

/// A DHCP lease, as the udhcpc script recorded it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DhcpLease {
    pub address: Option<Ipv4Prefix>,
    pub routers: Vec<Ipv4Addr>,
    pub dns_servers: Vec<Ipv4Addr>,
    pub domain: Option<String>,
    /// The DHCP server.
    pub server: Option<Ipv4Addr>,
    /// Seconds, as granted.
    pub lease_time: Option<u32>,
    /// Seconds until the address expires.
    pub remaining_time: Option<u32>,
}

impl Default for InterfaceState {
    fn default() -> Self {
        InterfaceState {
            admin_up: false,
            oper_status: "UNKNOWN",
            mac: None,
            in_octets: 0,
            in_pkts: 0,
            out_octets: 0,
            out_pkts: 0,
            addresses: BTreeMap::new(),
            dhcp_lease: None,
        }
    }
}

/// State data for the configuration in `applied`, as XML that clixon merges
/// into the configuration tree: several top-level elements, the interfaces
/// first. Interfaces missing from `states` are skipped, and so is what mstpd
/// reports while `stp` is None.
pub fn state_xml(
    applied: &DesiredState,
    states: &BTreeMap<String, InterfaceState>,
    stp: Option<&StpState>,
) -> String {
    use std::fmt::Write;

    let mut xml = String::from(r#"<interfaces xmlns="http://openconfig.net/yang/interfaces">"#);
    let interfaces = applied
        .ports
        .keys()
        .map(|name| (name, None))
        .chain(applied.svis.iter().map(|(name, svi)| (name, Some(svi))));
    for (name, svi) in interfaces {
        let Some(s) = states.get(name) else {
            continue;
        };
        let _ = write!(
            xml,
            "<interface><name>{}</name><state>\
             <admin-status>{}</admin-status><oper-status>{}</oper-status>\
             <counters><in-octets>{}</in-octets><in-pkts>{}</in-pkts>\
             <out-octets>{}</out-octets><out-pkts>{}</out-pkts></counters>\
             </state>",
            escape(name),
            if s.admin_up { "UP" } else { "DOWN" },
            s.oper_status,
            s.in_octets,
            s.in_pkts,
            s.out_octets,
            s.out_pkts,
        );
        if let (None, Some(mac)) = (svi, &s.mac) {
            let _ = write!(
                xml,
                r#"<ethernet xmlns="http://openconfig.net/yang/interfaces/ethernet"><state><hw-mac-address>{}</hw-mac-address></state></ethernet>"#,
                escape(mac)
            );
        }
        if let Some(svi) = svi {
            svi_ipv4_state_xml(&mut xml, svi, s);
        }
        xml.push_str("</interface>");
    }
    xml.push_str("</interfaces>");

    let _ = write!(
        xml,
        r#"<switch xmlns="{SWITCH_NS}"><state><vlan-mode>{}</vlan-mode></state></switch>"#,
        applied.mode.as_str()
    );

    let members = |vlan: u16| {
        applied
            .ports
            .iter()
            .filter(move |(_, port)| port.carries(vlan))
            .map(|(name, _)| escape(name))
    };
    let name_xml = |vlan: &Vlan| {
        vlan.name
            .as_deref()
            .map(|n| format!("<name>{}</name>", escape(n)))
            .unwrap_or_default()
    };
    match applied.mode {
        VlanMode::Dot1q if !applied.vlans.is_empty() => {
            let _ = write!(xml, r#"<vlans xmlns="{SWITCH_NS}">"#);
            for (id, vlan) in &applied.vlans {
                let _ = write!(
                    xml,
                    "<vlan><vlan-id>{id}</vlan-id><state><vlan-id>{id}</vlan-id>{}<status>{}</status></state><members>",
                    name_xml(vlan),
                    if vlan.active { "ACTIVE" } else { "SUSPENDED" }
                );
                for port in members(*id) {
                    let _ = write!(
                        xml,
                        "<member><state><interface>{port}</interface></state></member>"
                    );
                }
                xml.push_str("</members></vlan>");
            }
            xml.push_str("</vlans>");
        }
        VlanMode::PortBased if !applied.vlans.is_empty() => {
            let _ = write!(xml, r#"<port-based-vlans xmlns="{SWITCH_NS}">"#);
            for (id, vlan) in &applied.vlans {
                let _ = write!(
                    xml,
                    "<group><id>{id}</id><state><id>{id}</id>{}",
                    name_xml(vlan)
                );
                for port in members(*id) {
                    let _ = write!(xml, "<port>{port}</port>");
                }
                xml.push_str("</state></group>");
            }
            xml.push_str("</port-based-vlans>");
        }
        _ => {}
    }
    if let Some(applied_stp) = &applied.stp {
        stp::stp_state_xml(&mut xml, applied_stp, stp);
    }
    xml
}

/// routed-vlan/ipv4 state of an SVI: its addresses with their origin, whether
/// the DHCP client runs, and the lease.
fn svi_ipv4_state_xml(xml: &mut String, svi: &Svi, s: &InterfaceState) {
    use std::fmt::Write;

    xml.push_str(
        r#"<routed-vlan xmlns="http://openconfig.net/yang/vlan"><ipv4 xmlns="http://openconfig.net/yang/interfaces/ip"><addresses>"#,
    );
    for (prefix, origin) in &s.addresses {
        let _ = write!(
            xml,
            "<address><ip>{ip}</ip><state><ip>{ip}</ip><prefix-length>{}</prefix-length><origin>{}</origin></state></address>",
            prefix.prefix_len,
            origin.as_str(),
            ip = prefix.addr,
        );
    }
    let _ = write!(
        xml,
        "</addresses><state><dhcp-client>{}</dhcp-client>",
        svi.dhcp_client
    );
    if let (true, Some(lease)) = (svi.dhcp_client, &s.dhcp_lease) {
        let _ = write!(xml, r#"<dhcp-lease xmlns="{SWITCH_NS}">"#);
        if let Some(address) = lease.address {
            let _ = write!(
                xml,
                "<address>{}</address><prefix-length>{}</prefix-length>",
                address.addr, address.prefix_len
            );
        }
        for router in &lease.routers {
            let _ = write!(xml, "<router>{router}</router>");
        }
        for dns in &lease.dns_servers {
            let _ = write!(xml, "<dns-server>{dns}</dns-server>");
        }
        if let Some(domain) = &lease.domain {
            let _ = write!(xml, "<domain>{}</domain>", escape(domain));
        }
        if let Some(server) = lease.server {
            let _ = write!(xml, "<server>{server}</server>");
        }
        if let Some(t) = lease.lease_time {
            let _ = write!(xml, "<lease-time>{t}</lease-time>");
        }
        if let Some(t) = lease.remaining_time {
            let _ = write!(xml, "<remaining-time>{t}</remaining-time>");
        }
        xml.push_str("</dhcp-lease>");
    }
    xml.push_str("</state></ipv4></routed-vlan>");
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
