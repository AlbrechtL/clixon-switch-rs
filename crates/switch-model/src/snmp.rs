//! SNMP agent: `/snmp` of ietf-snmp (RFC 7407), run by net-snmp's snmpd.
//!
//! SNMPv3 only, and read-only: the engine with UDP listeners on IPv4, local
//! USM users with SHA authentication and optional AES privacy, and VACM
//! groups and views that grant read access. Communities, SNMPv1/v2c,
//! notifications, proxies, TLS/SSH transports, and write and notify views
//! are rejected, so SNMP never changes the configuration: the datastore
//! stays its only source.
//!
//! USM keys in ietf-snmp are localized keys (RFC 3414 A.2), tied to the
//! engine ID. snmpd takes them as they are.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::net::Ipv4Addr;

use crate::{opt_int, Error, SWITCH_NS};

const SNMP_NS: &str = "urn:ietf:params:xml:ns:yang:ietf-snmp";

/// Default UDP port of a command responder.
pub const SNMP_PORT: u16 = 161;

/// Octets of a localized key of HMAC-SHA-96.
const SHA_KEY_LEN: usize = 20;
/// Octets of a localized key of AES-128: the first 16 of the SHA
/// localization, which net-snmp also accepts whole.
const AES_KEY_LEN: usize = 16;

/// Net-SNMP's enterprise number, in engine IDs it derives.
const NET_SNMP_ENTERPRISE: u32 = 8072;

// ---------------------------------------------------------------------------
// RFC 7951 JSON
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct SnmpConfig {
    pub engine: Option<Engine>,
    pub usm: Option<Usm>,
    pub vacm: Option<Vacm>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Engine {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub listen: Vec<Listen>,
    /// Empty leaves v1, v2c and v3.
    pub version: Option<serde_json::Map<String, serde_json::Value>>,
    /// "80:00:1f:88:..."
    pub engine_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Listen {
    pub name: String,
    pub udp: Option<Udp>,
}

#[derive(Debug, Deserialize)]
pub struct Udp {
    pub ip: String,
    #[serde(default, deserialize_with = "opt_int")]
    pub port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Usm {
    pub local: Option<UsmLocal>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UsmLocal {
    #[serde(default)]
    pub user: Vec<User>,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub name: String,
    pub auth: Option<Auth>,
    #[serde(rename = "priv")]
    pub privacy: Option<Privacy>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Auth {
    pub sha: Option<Key>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Privacy {
    pub aes: Option<Key>,
}

#[derive(Debug, Deserialize)]
pub struct Key {
    /// Colon-separated hex octets.
    pub key: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Vacm {
    #[serde(default)]
    pub group: Vec<VacmGroup>,
    #[serde(default)]
    pub view: Vec<VacmView>,
}

#[derive(Debug, Deserialize)]
pub struct VacmGroup {
    pub name: String,
    #[serde(default)]
    pub member: Vec<Member>,
    #[serde(default)]
    pub access: Vec<Access>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Member {
    pub security_name: String,
    #[serde(default)]
    pub security_model: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Access {
    pub security_model: Option<String>,
    pub security_level: Option<String>,
    pub read_view: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VacmView {
    pub name: String,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

// ---------------------------------------------------------------------------
// Desired state
// ---------------------------------------------------------------------------

/// The SNMP agent as it should run. Present only while the engine is
/// enabled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snmp {
    /// None: derived from the bridge's MAC address, see [`default_engine_id`].
    pub engine_id: Option<Vec<u8>>,
    /// UDP listeners.
    pub listen: BTreeSet<(Ipv4Addr, u16)>,
    pub users: BTreeMap<String, UsmUser>,
    pub groups: BTreeMap<String, SnmpGroup>,
    pub views: BTreeMap<String, SnmpView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsmUser {
    /// Localized HMAC-SHA-96 key.
    pub auth_key: Vec<u8>,
    /// Localized AES-128 key, if the user has privacy.
    pub priv_key: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnmpGroup {
    /// USM user names.
    pub members: BTreeSet<String>,
    pub access: Vec<SnmpAccess>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnmpAccess {
    pub level: SecurityLevel,
    /// None: nothing is readable.
    pub read_view: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecurityLevel {
    AuthNoPriv,
    AuthPriv,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnmpView {
    /// Numeric OIDs, "1.3.6.1.2.1".
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

/// `/system/config`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemConfig {
    pub contact: Option<String>,
    pub location: Option<String>,
}

/// Validates `/snmp`, and returns the agent to run: None while the engine is
/// disabled. The rest of the configuration is validated then too.
pub(crate) fn desired_snmp(config: Option<&SnmpConfig>, errors: &mut Vec<Error>) -> Option<Snmp> {
    let config = config?;
    let mut err = |message: String| errors.push(Error::global(format!("snmp: {message}")));
    let engine = config.engine.as_ref();
    let enabled = engine.and_then(|e| e.enabled).unwrap_or(false);

    let mut snmp = Snmp::default();
    if let Some(engine) = engine {
        engine_config(engine, enabled, &mut snmp, &mut err);
    }
    let users = config
        .usm
        .as_ref()
        .and_then(|u| u.local.as_ref())
        .map_or(&[][..], |l| &l.user);
    for user in users {
        if let Some(u) = usm_user(user, &mut err) {
            snmp.users.insert(user.name.clone(), u);
        }
    }
    let vacm = config.vacm.as_ref();
    for view in vacm.map_or(&[][..], |v| &v.view) {
        if let Some(v) = vacm_view(view, &mut err) {
            snmp.views.insert(view.name.clone(), v);
        }
    }
    for group in vacm.map_or(&[][..], |v| &v.group) {
        if let Some(g) = vacm_group(group, &snmp, &mut err) {
            snmp.groups.insert(group.name.clone(), g);
        }
    }
    if enabled {
        if snmp.users.is_empty() {
            err("usm: a local user is required while the engine is enabled".into());
        }
        if snmp.groups.values().all(|g| g.access.is_empty()) {
            err("vacm: a group with access is required while the engine is enabled".into());
        }
    }
    enabled.then_some(snmp)
}

fn engine_config(engine: &Engine, enabled: bool, snmp: &mut Snmp, err: &mut impl FnMut(String)) {
    let versions = engine.version.as_ref();
    if enabled && !versions.is_some_and(|v| v.contains_key("v3")) {
        err("engine: version v3 is required".into());
    }
    for listen in &engine.listen {
        let Some(udp) = &listen.udp else {
            err(format!("engine listen {}: udp is required", listen.name));
            continue;
        };
        match udp.ip.parse::<Ipv4Addr>() {
            Ok(ip) => {
                snmp.listen.insert((ip, udp.port.unwrap_or(SNMP_PORT)));
            }
            Err(_) => err(format!(
                "engine listen {}: {} is not an IPv4 address",
                listen.name, udp.ip
            )),
        }
    }
    if enabled && snmp.listen.is_empty() {
        err("engine: a listen entry is required while the engine is enabled".into());
    }
    if let Some(id) = &engine.engine_id {
        match hex_octets(id) {
            Some(octets) if (5..=32).contains(&octets.len()) => snmp.engine_id = Some(octets),
            _ => err(format!(
                "engine: engine-id {id} is not 5 to 32 colon-separated hex octets"
            )),
        }
    }
}

fn usm_user(user: &User, err: &mut impl FnMut(String)) -> Option<UsmUser> {
    let name = &user.name;
    let mut ok = check_name("usm user", name, err);
    let key = |key: Option<&Key>, len: &[usize], what: &str, err: &mut dyn FnMut(String)| {
        let key = key?;
        match hex_octets(&key.key) {
            Some(octets) if len.contains(&octets.len()) => Some(octets),
            _ => {
                err(format!(
                    "usm user {name}: the localized {what} key must be {} colon-separated hex octets",
                    len.iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(" or ")
                ));
                None
            }
        }
    };
    let Some(auth_key) = key(
        user.auth.as_ref().and_then(|a| a.sha.as_ref()),
        &[SHA_KEY_LEN],
        "sha",
        err,
    ) else {
        if user.auth.as_ref().is_none_or(|a| a.sha.is_none()) {
            err(format!("usm user {name}: auth sha is required"));
        }
        return None;
    };
    let priv_key = match &user.privacy {
        None => None,
        Some(p) if p.aes.is_none() => {
            err(format!("usm user {name}: priv requires aes"));
            ok = false;
            None
        }
        Some(p) => {
            let key = key(p.aes.as_ref(), &[AES_KEY_LEN, SHA_KEY_LEN], "aes", err);
            ok &= key.is_some();
            key
        }
    };
    ok.then_some(UsmUser { auth_key, priv_key })
}

fn vacm_view(view: &VacmView, err: &mut impl FnMut(String)) -> Option<SnmpView> {
    let mut ok = check_name("vacm view", &view.name, err);
    let mut oids = |entries: &[String]| -> Vec<String> {
        entries
            .iter()
            .filter_map(|oid| match numeric_oid(oid) {
                Some(oid) => Some(oid),
                None => {
                    err(format!(
                        "vacm view {}: {oid} is not a numeric OID (no names or wildcards)",
                        view.name
                    ));
                    ok = false;
                    None
                }
            })
            .collect()
    };
    let include = oids(&view.include);
    let exclude = oids(&view.exclude);
    ok.then_some(SnmpView { include, exclude })
}

fn vacm_group(group: &VacmGroup, snmp: &Snmp, err: &mut impl FnMut(String)) -> Option<SnmpGroup> {
    let name = &group.name;
    let mut ok = check_name("vacm group", name, err);
    let mut out = SnmpGroup::default();
    for member in &group.member {
        let user = &member.security_name;
        if member.security_model.iter().any(|m| m != "usm") {
            err(format!(
                "vacm group {name} member {user}: only security-model usm is supported"
            ));
            ok = false;
        }
        if !snmp.users.contains_key(user) {
            err(format!(
                "vacm group {name}: member {user} is not a usm local user"
            ));
            ok = false;
        }
        out.members.insert(user.clone());
    }
    for access in &group.access {
        match access.security_model.as_deref() {
            Some("usm") | Some("any") => {}
            other => {
                err(format!(
                    "vacm group {name} access: security-model {} is not supported, only usm",
                    other.unwrap_or_default()
                ));
                ok = false;
            }
        }
        let level = match access.security_level.as_deref() {
            Some("auth-no-priv") => SecurityLevel::AuthNoPriv,
            Some("auth-priv") => SecurityLevel::AuthPriv,
            other => {
                err(format!(
                    "vacm group {name} access: security-level {} is not supported, only auth-no-priv and auth-priv",
                    other.unwrap_or_default()
                ));
                ok = false;
                continue;
            }
        };
        if let Some(view) = &access.read_view {
            if !snmp.views.contains_key(view) {
                err(format!(
                    "vacm group {name} access: read-view {view} does not exist"
                ));
                ok = false;
            }
        }
        if level == SecurityLevel::AuthPriv {
            for user in &out.members {
                if snmp.users.get(user).is_some_and(|u| u.priv_key.is_none()) {
                    err(format!(
                        "vacm group {name} access: security-level auth-priv, but member {user} has no priv key"
                    ));
                    ok = false;
                }
            }
        }
        out.access.push(SnmpAccess {
            level,
            read_view: access.read_view.clone(),
        });
    }
    ok.then_some(out)
}

/// snmpd's configuration file takes names as words.
fn check_name(what: &str, name: &str, err: &mut impl FnMut(String)) -> bool {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        err(format!(
            "{what} name \"{name}\" is not supported: only letters, digits, '-', '_' and '.'"
        ));
    }
    valid
}

/// Octets of "aa:bb:cc", or "aabbcc".
fn hex_octets(text: &str) -> Option<Vec<u8>> {
    let digits: String = text.chars().filter(|c| *c != ':').collect();
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok())
        .collect()
}

/// "1.3.6.1", also with a leading dot.
fn numeric_oid(text: &str) -> Option<String> {
    let oid = text.strip_prefix('.').unwrap_or(text);
    let valid = !oid.is_empty()
        && oid
            .split('.')
            .all(|part| !part.is_empty() && part.parse::<u32>().is_ok());
    valid.then(|| oid.to_string())
}

/// One line of text, as snmpd.conf takes it.
pub(crate) fn check_line(what: &str, value: &str, errors: &mut Vec<Error>) {
    if value.contains(|c: char| c.is_control()) {
        errors.push(Error::global(format!(
            "{what} must be one line without control characters"
        )));
    }
}

// ---------------------------------------------------------------------------
// snmpd
// ---------------------------------------------------------------------------

/// The engine ID snmpd derives from a MAC address (RFC 3411 format 3):
/// Net-SNMP's enterprise number with the top bit set, 3, the MAC.
pub fn default_engine_id(mac: [u8; 6]) -> Vec<u8> {
    let mut id = (NET_SNMP_ENTERPRISE | 0x8000_0000).to_be_bytes().to_vec();
    id.push(3);
    id.extend(mac);
    id
}

/// "aa:bb:cc:dd:ee:ff".
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let octets = hex_octets(text)?;
    (text.split(':').count() == 6).then_some(())?;
    octets.try_into().ok()
}

/// Colon-separated hex octets, as ietf-snmp writes an engine-id.
pub fn colon_hex(octets: &[u8]) -> String {
    octets
        .iter()
        .map(|o| format!("{o:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn hex(octets: &[u8]) -> String {
    octets.iter().map(|o| format!("{o:02x}")).collect()
}

/// What snmpd's configuration needs besides [`Snmp`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnmpdParams<'a> {
    /// The engine ID in use: [`Snmp::engine_id`] or [`default_engine_id`].
    pub engine_id: &'a [u8],
    /// AgentX master socket, for clixon_snmp.
    pub agentx_socket: &'a str,
    pub system: &'a SystemConfig,
    /// sysDescr; None: snmpd's default (uname).
    pub description: Option<&'a str>,
}

/// snmpd.conf for `snmp`. Every USM user comes from here: the plugin removes
/// those snmpd saved in its persistent file before it starts snmpd.
pub fn snmpd_conf(snmp: &Snmp, params: &SnmpdParams) -> String {
    let mut conf = String::from("# Written by the clixon-switch backend plugin from /snmp.\n");
    let listen: Vec<String> = snmp
        .listen
        .iter()
        .map(|(ip, port)| format!("udp:{ip}:{port}"))
        .collect();
    let _ = writeln!(conf, "agentaddress {}", listen.join(","));
    let _ = writeln!(conf, "exactEngineID 0x{}", hex(params.engine_id));
    // AgentX master for clixon_snmp, which serves the bridge MIBs.
    let _ = writeln!(conf, "master agentx");
    let _ = writeln!(conf, "agentXSocket unix:{}", params.agentx_socket);
    let _ = writeln!(conf, "agentXPerms 0600 0700");
    // sysServices: datalink/subnetwork (2).
    let _ = writeln!(conf, "sysServices 2");
    if let Some(description) = params.description {
        let _ = writeln!(conf, "sysDescr {description}");
    }
    // Set, even if empty: then SNMP cannot change them.
    let _ = writeln!(
        conf,
        "sysContact {}",
        params.system.contact.as_deref().unwrap_or_default()
    );
    let _ = writeln!(
        conf,
        "sysLocation {}",
        params.system.location.as_deref().unwrap_or_default()
    );

    for (name, user) in &snmp.users {
        let _ = write!(
            conf,
            "createUser -e 0x{} {name} SHA -l 0x{}",
            hex(params.engine_id),
            hex(&user.auth_key)
        );
        if let Some(key) = &user.priv_key {
            let _ = write!(conf, " AES -l 0x{}", hex(key));
        }
        conf.push('\n');
    }
    for (name, view) in &snmp.views {
        for oid in &view.include {
            let _ = writeln!(conf, "view {name} included .{oid}");
        }
        for oid in &view.exclude {
            let _ = writeln!(conf, "view {name} excluded .{oid}");
        }
    }
    for (name, group) in &snmp.groups {
        for user in &group.members {
            let _ = writeln!(conf, "group {name} usm {user}");
        }
        for access in &group.access {
            let level = match access.level {
                SecurityLevel::AuthNoPriv => "auth",
                SecurityLevel::AuthPriv => "priv",
            };
            let _ = writeln!(
                conf,
                "access {name} \"\" usm {level} exact {} none none",
                access.read_view.as_deref().unwrap_or("none")
            );
        }
    }
    conf
}

/// Removes the USM users from the text of snmpd's persistent file, and keeps
/// the rest (engineBoots, oldEngineID, ...). snmpd saves every user there,
/// and would bring back users that the configuration no longer has.
pub fn strip_persistent_users(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("usmUser "))
        .fold(String::new(), |mut out, line| {
            out.push_str(line);
            out.push('\n');
            out
        })
}

/// State data of `/snmp`: the engine ID in use.
pub fn snmp_state_xml(engine_id: &[u8]) -> String {
    format!(
        r#"<snmp xmlns="{SNMP_NS}"><engine><engine-id-in-use xmlns="{SWITCH_NS}">{}</engine-id-in-use></engine></snmp>"#,
        colon_hex(engine_id)
    )
}
