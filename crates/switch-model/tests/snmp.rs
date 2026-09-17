//! `/snmp`: what is accepted, what is rejected, and the snmpd configuration.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use serde_json::{json, Value};
use switch_model::{
    default_engine_id, snmp_state_xml, snmpd_conf, strip_persistent_users, validate, SecurityLevel,
    SnmpdParams, SystemConfig,
};

const FACTORY_DEFAULT: &str = include_str!("data/factory-default.json");

const AUTH_KEY: &str = "61:27:5e:7f:05:5c:63:11:09:6a:1f:c0:ec:1c:78:1a:cf:5e:23:d8";
const PRIV_KEY: &str = "41:f5:c6:d4:a6:dd:41:b7:c0:cb:1c:ef:24:dd:d2:5f";

fn ports() -> BTreeSet<String> {
    (1..=8).map(|i| format!("lan{i}")).collect()
}

/// The factory default with SNMP enabled, as clixon prints a transaction
/// tree: with the YANG defaults filled in.
fn config() -> Value {
    let mut tree: Value = serde_json::from_str(FACTORY_DEFAULT).unwrap();
    tree["ietf-snmp:snmp"] = json!({
        "engine": {
            "enabled": true,
            "listen": [{"name": "all", "udp": {"ip": "0.0.0.0"}}],
            "version": {"v3": [null]},
            "engine-id": "80:00:1f:88:04:74:65:73:74",
            "enable-authen-traps": false
        },
        "usm": {"local": {"user": [
            {"name": "nms", "auth": {"sha": {"key": AUTH_KEY}}, "priv": {"aes": {"key": PRIV_KEY}}},
            {"name": "monitor", "auth": {"sha": {"key": AUTH_KEY}}}
        ]}},
        "vacm": {
            "group": [
                {"name": "admins",
                 "member": [{"security-name": "nms", "security-model": ["usm"]}],
                 "access": [{"context": "", "context-match": "exact", "security-model": "usm",
                             "security-level": "auth-priv", "read-view": "all"}]},
                {"name": "monitors",
                 "member": [{"security-name": "monitor", "security-model": ["usm"]}],
                 "access": [{"context": "", "context-match": "exact", "security-model": "any",
                             "security-level": "auth-no-priv", "read-view": "bridge"}]}
            ],
            "view": [
                {"name": "all", "include": ["1.3.6.1"]},
                {"name": "bridge", "include": [".1.3.6.1.2.1.17", "1.3.6.1.2.1.1"],
                 "exclude": ["1.3.6.1.2.1.17.4"]}
            ]
        }
    });
    tree["clixon-switch:system"] =
        json!({"config": {"contact": "noc@example.com", "location": "rack 3"}});
    tree
}

/// Validates `config()` after `change`, expecting it to be rejected. The
/// errors, one per line: a user left out makes its group members unknown
/// too, and so on.
fn rejected(change: impl FnOnce(&mut Value)) -> String {
    let mut tree = config();
    change(&mut tree);
    let errors = validate(&tree.to_string(), &ports())
        .expect_err("must be rejected")
        .0;
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// An expected error, and the change to `config()` that causes it.
type Case = (&'static str, fn(&mut Value));

fn snmp(tree: &mut Value) -> &mut Value {
    &mut tree["ietf-snmp:snmp"]
}

#[test]
fn enabled() {
    let state = validate(&config().to_string(), &ports()).unwrap();
    let snmp = state.snmp.expect("SNMP enabled");
    assert_eq!(
        snmp.engine_id,
        Some(vec![0x80, 0x00, 0x1f, 0x88, 0x04, b't', b'e', b's', b't'])
    );
    assert_eq!(snmp.listen, BTreeSet::from([(Ipv4Addr::UNSPECIFIED, 161)]));
    assert_eq!(snmp.users["nms"].auth_key.len(), 20);
    assert_eq!(snmp.users["nms"].priv_key.as_ref().map(Vec::len), Some(16));
    assert_eq!(snmp.users["monitor"].priv_key, None);
    assert_eq!(
        snmp.groups["monitors"].access[0].level,
        SecurityLevel::AuthNoPriv
    );
    assert_eq!(
        snmp.views["bridge"].include,
        ["1.3.6.1.2.1.17", "1.3.6.1.2.1.1"]
    );
    assert_eq!(state.system.location.as_deref(), Some("rack 3"));
}

#[test]
fn off_by_default_and_when_disabled() {
    let factory: Value = serde_json::from_str(FACTORY_DEFAULT).unwrap();
    assert_eq!(validate(&factory.to_string(), &ports()).unwrap().snmp, None);

    let mut tree = config();
    snmp(&mut tree)["engine"]["enabled"] = json!(false);
    assert_eq!(validate(&tree.to_string(), &ports()).unwrap().snmp, None);
    // Still validated.
    snmp(&mut tree)["usm"]["local"]["user"][0]["auth"]["sha"]["key"] = json!("00:11");
    assert!(validate(&tree.to_string(), &ports()).is_err());
}

#[test]
fn snmpd_configuration() {
    let state = validate(&config().to_string(), &ports()).unwrap();
    let snmp = state.snmp.unwrap();
    let engine_id = snmp.engine_id.clone().unwrap();
    let conf = snmpd_conf(
        &snmp,
        &SnmpdParams {
            engine_id: &engine_id,
            agentx_socket: "/var/run/clixon-switch/agentx.sock",
            system: &state.system,
            description: Some("ethernet-switch-os 1.0"),
        },
    );
    let expected = "# Written by the clixon-switch backend plugin from /snmp.
agentaddress udp:0.0.0.0:161
exactEngineID 0x80001f880474657374
master agentx
agentXSocket unix:/var/run/clixon-switch/agentx.sock
agentXPerms 0600 0700
sysServices 2
sysDescr ethernet-switch-os 1.0
sysContact noc@example.com
sysLocation rack 3
createUser -e 0x80001f880474657374 monitor SHA -l 0x61275e7f055c6311096a1fc0ec1c781acf5e23d8
createUser -e 0x80001f880474657374 nms SHA -l 0x61275e7f055c6311096a1fc0ec1c781acf5e23d8 AES -l 0x41f5c6d4a6dd41b7c0cb1cef24ddd25f
view all included .1.3.6.1
view bridge included .1.3.6.1.2.1.17
view bridge included .1.3.6.1.2.1.1
view bridge excluded .1.3.6.1.2.1.17.4
group admins usm nms
access admins \"\" usm priv exact all none none
group monitors usm monitor
access monitors \"\" usm auth exact bridge none none
";
    assert_eq!(conf, expected);
}

#[test]
fn contact_and_location_empty_by_default() {
    let mut tree = config();
    tree.as_object_mut().unwrap().remove("clixon-switch:system");
    let state = validate(&tree.to_string(), &ports()).unwrap();
    assert_eq!(state.system, SystemConfig::default());
    let snmp = state.snmp.unwrap();
    let conf = snmpd_conf(
        &snmp,
        &SnmpdParams {
            engine_id: &[1, 2, 3, 4, 5],
            agentx_socket: "/sock",
            system: &state.system,
            description: None,
        },
    );
    assert!(conf.contains("\nsysContact \nsysLocation \n"), "{conf}");
    assert!(!conf.contains("sysDescr"));
}

#[test]
fn unsupported_nodes() {
    let cases: [Case; 7] = [
        ("snmp/engine/version/v2c is not supported", |t| {
            snmp(t)["engine"]["version"]["v2c"] = json!([null])
        }),
        ("snmp/community is not supported", |t| {
            snmp(t)["community"] =
                json!([{"index": "p", "text-name": "public", "security-name": "nms"}])
        }),
        ("snmp/usm/local/user/auth/md5 is not supported", |t| {
            snmp(t)["usm"]["local"]["user"][1]["auth"] = json!({"md5": {"key": AUTH_KEY}})
        }),
        ("snmp/vacm/group/access/write-view is not supported", |t| {
            snmp(t)["vacm"]["group"][0]["access"][0]["write-view"] = json!("all")
        }),
        ("snmp/target is not supported", |t| {
            snmp(t)["target"] = json!([{"name": "nms", "udp": {"ip": "192.0.2.1"}}])
        }),
        (
            "snmp/engine/enable-authen-traps true is not supported, only false",
            |t| snmp(t)["engine"]["enable-authen-traps"] = json!(true),
        ),
        (
            "snmp/vacm/group/access/context admin is not supported, only ",
            |t| snmp(t)["vacm"]["group"][0]["access"][0]["context"] = json!("admin"),
        ),
    ];
    for (expected, change) in cases {
        let errors = rejected(change);
        assert!(
            errors.lines().any(|e| e == expected),
            "{expected}\n  not in\n{errors}"
        );
    }
}

#[test]
fn invalid_settings() {
    let cases: [Case; 11] = [
        ("snmp: engine: version v3 is required", |t| {
            snmp(t)["engine"]["version"] = json!({})
        }),
        (
            "snmp: engine: a listen entry is required while the engine is enabled",
            |t| snmp(t)["engine"]["listen"] = json!([]),
        ),
        ("snmp: engine listen all: ::1 is not an IPv4 address", |t| {
            snmp(t)["engine"]["listen"][0]["udp"]["ip"] = json!("::1")
        }),
        (
            "snmp: usm user nms: the localized sha key must be 20 colon-separated hex octets",
            |t| snmp(t)["usm"]["local"]["user"][0]["auth"]["sha"]["key"] = json!("00:11:22"),
        ),
        (
            "snmp: usm user nms: the localized aes key must be 16 or 20 colon-separated hex octets",
            |t| snmp(t)["usm"]["local"]["user"][0]["priv"]["aes"]["key"] = json!("00:11:22"),
        ),
        (
            "snmp: vacm view all: 1.3.* is not a numeric OID (no names or wildcards)",
            |t| snmp(t)["vacm"]["view"][0]["include"] = json!(["1.3.*"]),
        ),
        (
            "snmp: vacm group monitors: member guest is not a usm local user",
            |t| {
                snmp(t)["vacm"]["group"][1]["member"][0]["security-name"] = json!("guest")
            },
        ),
        (
            "snmp: vacm group monitors access: read-view everything does not exist",
            |t| snmp(t)["vacm"]["group"][1]["access"][0]["read-view"] = json!("everything"),
        ),
        (
            "snmp: vacm group monitors access: security-level auth-priv, but member monitor has no priv key",
            |t| {
                snmp(t)["vacm"]["group"][1]["access"][0]["security-level"] = json!("auth-priv")
            },
        ),
        (
            "snmp: vacm group admins access: security-level no-auth-no-priv is not supported, only auth-no-priv and auth-priv",
            |t| {
                snmp(t)["vacm"]["group"][0]["access"][0]["security-level"] =
                    json!("no-auth-no-priv")
            },
        ),
        ("system contact must be one line without control characters", |t| {
            t["clixon-switch:system"]["config"]["contact"] = json!("noc\nroot")
        }),
    ];
    for (expected, change) in cases {
        let errors = rejected(change);
        assert!(
            errors.lines().any(|e| e == expected),
            "{expected}\n  not in\n{errors}"
        );
    }
}

#[test]
fn names_snmpd_can_parse() {
    let error = rejected(|t| {
        let user = &mut snmp(t)["usm"]["local"]["user"][1];
        user["name"] = json!("net ops");
        snmp(t)["vacm"]["group"][1]["member"][0]["security-name"] = json!("net ops");
    });
    assert!(
        error.starts_with("snmp: usm user name \"net ops\" is not supported"),
        "{error}"
    );
}

#[test]
fn engine_id_from_mac() {
    assert_eq!(
        default_engine_id([0x02, 0, 0, 0, 0, 0x01]),
        [0x80, 0x00, 0x1f, 0x88, 0x03, 0x02, 0, 0, 0, 0, 0x01]
    );
    assert_eq!(
        snmp_state_xml(&default_engine_id([0x02, 0, 0, 0, 0, 0x01])),
        r#"<snmp xmlns="urn:ietf:params:xml:ns:yang:ietf-snmp"><engine><engine-id-in-use xmlns="urn:github:albrechtl:clixon-switch">80:00:1f:88:03:02:00:00:00:00:01</engine-id-in-use></engine></snmp>"#
    );
}

#[test]
fn persistent_users_are_dropped() {
    let text = "engineBoots 3\n\
                usmUser 1 3 0x80001f88 \"nms\" \"nms\" NULL .1.3.6.1.6.3.10.1.1.3 0x6127 .1.3.6.1.6.3.10.1.2.4 0x41f5 \"\"\n\
                oldEngineID 0x80001f88\n";
    assert_eq!(
        strip_persistent_users(text),
        "engineBoots 3\noldEngineID 0x80001f88\n"
    );
}
