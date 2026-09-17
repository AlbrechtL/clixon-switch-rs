//! The configuration the switch implements.
//!
//! clixon accepts whatever the YANG modules allow. `deviate not-supported`
//! would be the YANG way to narrow that down, but clixon 7.8 only skips
//! `must` checks for such nodes and still accepts data for them. So the
//! plugin rejects what it does not implement itself: every configuration
//! node below is either implemented ([`Rule::Any`]), or accepted only with its
//! YANG default ([`Rule::Default`]), because clixon fills defaults into the
//! tree. Operational state, which clixon merges into the tree unless
//! CLICON_VALIDATE_TARGET_STATE is off (clixon.xml turns it off), and the
//! top-level modules other than openconfig-interfaces, clixon-switch and
//! openconfig-spanning-tree (NACM, YANG library, ...) are not checked.
//!
//! Implementing more of the model means moving leaves from `Default` to
//! `Any` and adding containers here.

use serde_json::Value;

use crate::Error;

enum Rule {
    /// Implemented: any value (types and ranges are clixon's business).
    Any,
    /// Not implemented: accepted with this YANG default only.
    Default(&'static str),
    /// A container with these members. Members not listed are not
    /// implemented.
    Node(&'static [(&'static str, Rule)]),
    /// A list whose entries have these members.
    List(&'static [(&'static str, Rule)]),
    /// Operational state.
    Ignore,
}

use Rule::{Any, Default, Ignore, List, Node};

const STATE: (&str, Rule) = ("state", Ignore);

/// Members of `/interfaces/interface`.
const INTERFACE: &[(&str, Rule)] = &[
    ("name", Any),
    (
        "config",
        Node(&[
            ("name", Any),
            ("type", Any),
            ("enabled", Any),
            ("description", Any),
            ("loopback-mode", Default("NONE")),
            ("openconfig-vlan:tpid", Default("TPID_0X8100")),
        ]),
    ),
    STATE,
    (
        "hold-time",
        Node(&[
            (
                "config",
                Node(&[("up", Default("0")), ("down", Default("0"))]),
            ),
            STATE,
        ]),
    ),
    (
        "penalty-based-aied",
        Node(&[
            (
                "config",
                Node(&[
                    ("max-suppress-time", Default("0")),
                    ("decay-half-life", Default("0")),
                    ("suppress-threshold", Default("0")),
                    ("reuse-threshold", Default("0")),
                    ("flap-penalty", Default("0")),
                ]),
            ),
            STATE,
        ]),
    ),
    ("openconfig-if-ethernet:ethernet", Node(ETHERNET)),
    ("openconfig-vlan:routed-vlan", Node(ROUTED_VLAN)),
];

const ETHERNET: &[(&str, Rule)] = &[
    (
        "config",
        Node(&[
            ("enable-flow-control", Default("false")),
            ("auto-negotiate", Default("true")),
            ("standalone-link-training", Default("false")),
        ]),
    ),
    STATE,
    (
        "openconfig-vlan:switched-vlan",
        Node(&[
            (
                "config",
                Node(&[
                    ("interface-mode", Any),
                    ("access-vlan", Any),
                    ("native-vlan", Any),
                    ("trunk-vlans", Any),
                ]),
            ),
            STATE,
        ]),
    ),
];

const ROUTED_VLAN: &[(&str, Rule)] = &[
    ("config", Node(&[("vlan", Any)])),
    STATE,
    ("openconfig-if-ip:ipv4", Node(IPV4)),
    ("openconfig-if-ip:ipv6", Node(IPV6)),
];

const IPV4: &[(&str, Rule)] = &[
    (
        "addresses",
        Node(&[(
            "address",
            List(&[
                ("ip", Any),
                (
                    "config",
                    Node(&[
                        ("ip", Any),
                        ("prefix-length", Any),
                        ("type", Default("PRIMARY")),
                    ]),
                ),
                STATE,
            ]),
        )]),
    ),
    (
        "config",
        Node(&[("enabled", Default("true")), ("dhcp-client", Any)]),
    ),
    STATE,
    (
        "proxy-arp",
        Node(&[("config", Node(&[("mode", Default("DISABLE"))])), STATE]),
    ),
    (
        "unnumbered",
        Node(&[("config", Node(&[("enabled", Default("false"))])), STATE]),
    ),
    (
        "urpf",
        Node(&[("config", Node(&[("enabled", Default("false"))])), STATE]),
    ),
];

const IPV6: &[(&str, Rule)] = &[
    (
        "config",
        Node(&[
            ("enabled", Default("true")),
            ("dup-addr-detect-transmits", Default("1")),
            ("learn-unsolicited", Default("NONE")),
            ("dhcp-client", Default("false")),
        ]),
    ),
    STATE,
    (
        "router-advertisement",
        Node(&[
            (
                "config",
                Node(&[
                    ("enable", Default("true")),
                    ("suppress", Default("false")),
                    ("mode", Default("ALL")),
                    ("managed", Default("false")),
                    ("other-config", Default("false")),
                ]),
            ),
            STATE,
        ]),
    ),
    (
        "unnumbered",
        Node(&[("config", Node(&[("enabled", Default("false"))])), STATE]),
    ),
    (
        "urpf",
        Node(&[("config", Node(&[("enabled", Default("false"))])), STATE]),
    ),
];

/// Cost and priority of the ports in one spanning tree: members of an
/// `interfaces` container.
const STP_TREE_INTERFACES: &[(&str, Rule)] = &[(
    "interface",
    List(&[
        ("name", Any),
        (
            "config",
            Node(&[("name", Any), ("cost", Any), ("port-priority", Any)]),
        ),
        STATE,
    ]),
)];

/// `/stp`. Not implemented: rapid-pvst, bridge assurance, EtherChannel guard,
/// loop guard, and BPDU guard recovery.
const STP: &[(&str, Rule)] = &[
    (
        "global",
        Node(&[
            (
                "config",
                Node(&[
                    ("enabled-protocol", Any),
                    ("bridge-assurance", Default("false")),
                    ("etherchannel-misconfig-guard", Default("false")),
                    ("loop-guard", Default("false")),
                    ("bpdu-guard", Any),
                    ("bpdu-filter", Any),
                ]),
            ),
            STATE,
        ]),
    ),
    (
        "rstp",
        Node(&[
            (
                "config",
                Node(&[
                    ("hello-time", Any),
                    ("max-age", Any),
                    ("forwarding-delay", Any),
                    ("hold-count", Any),
                    ("bridge-priority", Any),
                ]),
            ),
            STATE,
            ("interfaces", Node(STP_TREE_INTERFACES)),
        ]),
    ),
    (
        "mstp",
        Node(&[
            (
                "config",
                Node(&[
                    ("name", Any),
                    ("revision", Any),
                    ("max-hop", Any),
                    ("hello-time", Any),
                    ("max-age", Any),
                    ("forwarding-delay", Any),
                    ("hold-count", Any),
                    ("clixon-switch:bridge-priority", Any),
                ]),
            ),
            STATE,
            (
                "mst-instances",
                Node(&[(
                    "mst-instance",
                    List(&[
                        ("mst-id", Any),
                        (
                            "config",
                            Node(&[("mst-id", Any), ("vlan", Any), ("bridge-priority", Any)]),
                        ),
                        STATE,
                        ("interfaces", Node(STP_TREE_INTERFACES)),
                    ]),
                )]),
            ),
            ("clixon-switch:interfaces", Node(STP_TREE_INTERFACES)),
        ]),
    ),
    (
        "interfaces",
        Node(&[(
            "interface",
            List(&[
                ("name", Any),
                (
                    "config",
                    Node(&[
                        ("name", Any),
                        ("edge-port", Any),
                        ("link-type", Any),
                        ("guard", Any),
                        ("bpdu-guard", Any),
                        ("bpdu-filter", Any),
                    ]),
                ),
                STATE,
            ]),
        )]),
    ),
];

/// The top-level containers besides `/interfaces`: the clixon-switch
/// module's, all implemented, and `/stp`.
const TOP: &[(&str, Rule)] = &[
    (
        "clixon-switch:vlans",
        Node(&[(
            "vlan",
            List(&[
                ("vlan-id", Any),
                (
                    "config",
                    Node(&[("vlan-id", Any), ("name", Any), ("status", Any)]),
                ),
                STATE,
                ("members", Ignore),
            ]),
        )]),
    ),
    (
        "clixon-switch:switch",
        Node(&[("config", Node(&[("vlan-mode", Any)])), STATE]),
    ),
    ("clixon-switch:system", Node(&[STATE])),
    (
        "clixon-switch:port-based-vlans",
        Node(&[(
            "group",
            List(&[
                ("id", Any),
                ("config", Node(&[("id", Any), ("name", Any), ("port", Any)])),
                STATE,
            ]),
        )]),
    ),
    ("openconfig-spanning-tree:stp", Node(STP)),
];

/// One error per configured node in `config` (the RFC 7951 JSON of a
/// datastore tree) that the switch does not implement.
pub(crate) fn check(config: &Value) -> Vec<Error> {
    let interfaces = config
        .get("openconfig-interfaces:interfaces")
        .and_then(|i| i.get("interface"))
        .and_then(Value::as_array);

    let mut errors = Vec::new();
    for interface in interfaces.into_iter().flatten() {
        let name = interface.get("name").and_then(Value::as_str).unwrap_or("?");
        let mut messages = Vec::new();
        check_members(interface, INTERFACE, "", &mut messages);
        errors.extend(messages.into_iter().map(|message| Error {
            interface: name.to_string(),
            message,
        }));
    }

    for (key, rule) in TOP {
        if let Some(value) = config.get(key) {
            let mut messages = Vec::new();
            check_value(
                value,
                rule,
                key.rsplit(':').next().unwrap_or(key),
                &mut messages,
            );
            errors.extend(messages.into_iter().map(|message| Error {
                interface: String::new(),
                message,
            }));
        }
    }
    errors
}

fn check_members(value: &Value, rules: &[(&str, Rule)], path: &str, messages: &mut Vec<String>) {
    // Wrong shapes are reported when the tree is deserialized.
    let Some(members) = value.as_object() else {
        return;
    };
    for (key, child) in members {
        // Paths without module prefixes, e.g. "routed-vlan/ipv6/config".
        let name = key.rsplit(':').next().unwrap_or(key);
        let child_path = if path.is_empty() {
            name.to_string()
        } else {
            format!("{path}/{name}")
        };
        match rules.iter().find(|(member, _)| member == key) {
            None => messages.push(format!("{child_path} is not supported")),
            Some((_, rule)) => check_value(child, rule, &child_path, messages),
        }
    }
}

fn check_value(value: &Value, rule: &Rule, path: &str, messages: &mut Vec<String>) {
    match rule {
        Any | Ignore => {}
        Default(default) => {
            if !is_value(value, default) {
                let shown = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                messages.push(format!("{path} {shown} is not supported, only {default}"));
            }
        }
        Node(rules) => check_members(value, rules, path, messages),
        List(rules) => {
            for entry in value.as_array().into_iter().flatten() {
                check_members(entry, rules, path, messages);
            }
        }
    }
}

/// Compares a leaf with a default written as text. Identities may carry a
/// module prefix, numbers may arrive quoted.
fn is_value(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(s) => s == expected || s.rsplit(':').next() == Some(expected),
        Value::Bool(b) => b.to_string() == expected,
        Value::Number(n) => n.to_string() == expected,
        _ => false,
    }
}
