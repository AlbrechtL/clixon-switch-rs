use switch_model::{
    date_and_time, parse_loadavg, parse_meminfo, parse_os_release, parse_uptime, system_state_xml,
    SystemState,
};

#[test]
fn os_release() {
    let text = "ID=ethernet-switch-os\nNAME=\"Ethernet Switch OS\"\nVERSION=\"6.0.3 (wrynose)\"\nVERSION_ID=6.0.3\n";
    assert_eq!(
        parse_os_release(text),
        (
            Some("Ethernet Switch OS".to_string()),
            Some("6.0.3 (wrynose)".to_string())
        )
    );
    assert_eq!(parse_os_release(""), (None, None));
}

#[test]
fn proc_files() {
    assert_eq!(parse_uptime("12345.67 23456.78\n"), Some(12345));
    assert_eq!(parse_uptime(""), None);
    assert_eq!(
        parse_loadavg("0.08 0.03 0.01 1/52 1234\n"),
        Some(["0.08".to_string(), "0.03".to_string(), "0.01".to_string()])
    );
    assert_eq!(parse_loadavg("0.08 x"), None);
    let meminfo =
        "MemTotal:         124680 kB\nMemFree:           80000 kB\nMemAvailable:      98012 kB\n";
    assert_eq!(parse_meminfo(meminfo), (Some(124_680), Some(98_012)));
}

#[test]
fn dates() {
    assert_eq!(date_and_time(0), "1970-01-01T00:00:00Z");
    assert_eq!(date_and_time(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(date_and_time(1_789_561_845), "2026-09-16T12:30:45Z");
}

#[test]
fn xml() {
    let system = SystemState {
        hostname: Some("gs<1900>".into()),
        os_name: Some("Ethernet Switch OS".into()),
        uptime: Some(42),
        load_average: Some(["0.08".into(), "0.03".into(), "0.01".into()]),
        current_time: Some(0),
        ..SystemState::default()
    };
    assert_eq!(
        system_state_xml(&system),
        r#"<system xmlns="urn:github:albrechtl:clixon-switch"><state><hostname>gs&lt;1900&gt;</hostname><os-name>Ethernet Switch OS</os-name><current-datetime>1970-01-01T00:00:00Z</current-datetime><uptime>42</uptime><load-average-1>0.08</load-average-1><load-average-5>0.03</load-average-5><load-average-15>0.01</load-average-15></state></system>"#
    );
    assert_eq!(
        system_state_xml(&SystemState::default()),
        r#"<system xmlns="urn:github:albrechtl:clixon-switch"><state></state></system>"#
    );
}
