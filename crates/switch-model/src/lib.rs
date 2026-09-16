//! The part of the OpenConfig model that the switch implements, and its
//! translation into a validated [`DesiredState`].
//!
//! The input is the RFC 7951 JSON that clixon prints for a datastore tree
//! (`xml2json_cbuf_vec` with `skiptop`). Only what the plugin acts on is
//! modelled; serde skips everything else. clixon has already validated the
//! tree against YANG (types, ranges, mandatory leaves, deviations), so the
//! checks here are the ones YANG cannot express: that a port exists on this
//! switch, one routed VLAN interface per VLAN, and so on.

mod supported;

use serde::{de, Deserialize, Deserializer};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::Ipv4Addr;

/// Linux bridge that holds all switch ports. It is an implementation detail,
/// not part of the model, so no interface may be configured with this name.
pub const BRIDGE_NAME: &str = "br-lan";

/// IFNAMSIZ - 1.
const MAX_IFNAME_LEN: usize = 15;

// ---------------------------------------------------------------------------
// RFC 7951 JSON
// ---------------------------------------------------------------------------

/// Top of a datastore tree.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(rename = "openconfig-interfaces:interfaces")]
    pub interfaces: Option<Interfaces>,
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

/// A leaf that may arrive as a JSON number or a string. RFC 7951 quotes
/// only 64-bit integers, but numeric strings are accepted as well.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    Number(u64),
    String(String),
}

impl Scalar {
    fn as_u64(&self) -> Option<u64> {
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
    /// Switch ports by name. Ports of the switch that are missing here are
    /// taken out of the bridge and set down.
    pub ports: BTreeMap<String, Port>,
    /// Routed VLAN interfaces by name.
    pub svis: BTreeMap<String, Svi>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Port {
    pub enabled: bool,
    /// Untagged VLAN and PVID of the port.
    pub access_vlan: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Svi {
    pub enabled: bool,
    pub vlan: u16,
    pub addresses: BTreeSet<Ipv4Prefix>,
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
    pub interface: String,
    pub message: String,
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
    let parse_error = |e: serde_json::Error| Error {
        interface: String::new(),
        message: format!("cannot parse the configuration: {e}"),
    };
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
    let mut state = DesiredState::default();
    let mut errors = Vec::new();

    let interfaces = config.interfaces.as_ref().map_or(&[][..], |i| &i.interface);
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
        match if_type.map(|t| t.rsplit(':').next().unwrap_or(t)) {
            Some("ethernetCsmacd") => match port(interface, switch_ports) {
                Ok(p) => {
                    state.ports.insert(interface.name.clone(), p);
                }
                Err(messages) => messages.into_iter().for_each(&mut err),
            },
            Some("l3ipvlan") => match svi(interface, switch_ports) {
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

    check_unique_vlans(&state, &mut errors);
    check_unique_addresses(&state, &mut errors);

    if errors.is_empty() {
        Ok(state)
    } else {
        Err(Errors(errors))
    }
}

fn port(interface: &Interface, switch_ports: &BTreeSet<String>) -> Result<Port, Vec<String>> {
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
    let access_vlan = match vlan_config {
        None => {
            errors.push("ethernet/switched-vlan/config is required: interface-mode ACCESS and an access-vlan".into());
            None
        }
        Some(c) => match (c.interface_mode.as_deref(), c.access_vlan) {
            (Some("TRUNK"), _) => {
                errors.push("interface-mode TRUNK is not supported yet".into());
                None
            }
            (Some("ACCESS") | None, Some(vlan)) => Some(vlan),
            (Some("ACCESS") | None, None) => {
                errors.push("switched-vlan access-vlan is required".into());
                None
            }
            (Some(mode), _) => {
                errors.push(format!("interface-mode {mode} is not supported"));
                None
            }
        },
    };
    if let Some(vlan) = access_vlan {
        if let Err(e) = check_vlan_id(u64::from(vlan)) {
            errors.push(e);
        }
    }

    match access_vlan {
        Some(access_vlan) if errors.is_empty() => Ok(Port {
            enabled: interface.config.enabled.unwrap_or(true),
            access_vlan,
        }),
        _ => Err(errors),
    }
}

fn svi(interface: &Interface, switch_ports: &BTreeSet<String>) -> Result<Svi, Vec<String>> {
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
        None => {
            errors.push("routed-vlan/config/vlan is required".into());
            None
        }
        Some(v) => match v.as_u64() {
            None => {
                errors.push(format!(
                    "routed-vlan vlan \"{v}\": VLAN names are not supported, use the VLAN id"
                ));
                None
            }
            Some(id) => match check_vlan_id(id) {
                Ok(id) => Some(id),
                Err(e) => {
                    errors.push(e);
                    None
                }
            },
        },
    };

    let mut addresses = BTreeSet::new();
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
        }),
        _ => Err(errors),
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
        }
    }
}

/// State data for the interfaces in `applied`, as XML that clixon merges into
/// the configuration tree. Interfaces missing from `states` are skipped.
pub fn state_xml(applied: &DesiredState, states: &BTreeMap<String, InterfaceState>) -> String {
    use std::fmt::Write;

    let mut xml = String::from(r#"<interfaces xmlns="http://openconfig.net/yang/interfaces">"#);
    let interfaces = applied
        .ports
        .keys()
        .map(|name| (name, true))
        .chain(applied.svis.keys().map(|name| (name, false)));
    for (name, is_port) in interfaces {
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
        if let (true, Some(mac)) = (is_port, &s.mac) {
            let _ = write!(
                xml,
                r#"<ethernet xmlns="http://openconfig.net/yang/interfaces/ethernet"><state><hw-mac-address>{}</hw-mac-address></state></ethernet>"#,
                escape(mac)
            );
        }
        xml.push_str("</interface>");
    }
    xml.push_str("</interfaces>");
    xml
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
