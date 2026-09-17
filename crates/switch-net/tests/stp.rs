use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io;
use std::rc::Rc;

use serde_json::{json, Value};
use switch_model::{desired_state, Config, Stp};
use switch_net::dhcp::Processes;
use switch_net::stp::{commands, Control, Event, Mstpd, MstpdConfig};

/// Processes and mstpctl runs that only record what happens. Clones share
/// the record.
#[derive(Default, Clone)]
struct Fake(Rc<RefCell<Record>>);

#[derive(Default)]
struct Record {
    next_pid: u32,
    running: BTreeSet<u32>,
    /// mstpctl arguments, without the binary.
    ran: Vec<Vec<String>>,
    /// mstpctl commands (first argument) that fail.
    failing: BTreeSet<String>,
}

impl Processes for Fake {
    fn spawn(&mut self, argv: &[String], _env: &[(String, String)]) -> io::Result<u32> {
        assert_eq!(argv, ["/usr/sbin/mstpd", "-d", "-s"]);
        let mut r = self.0.borrow_mut();
        r.next_pid += 1;
        let pid = r.next_pid;
        r.running.insert(pid);
        Ok(pid)
    }

    fn running(&mut self, pid: u32) -> bool {
        self.0.borrow().running.contains(&pid)
    }

    fn stop(&mut self, pid: u32) {
        assert!(self.0.borrow_mut().running.remove(&pid));
    }
}

impl Control for Fake {
    fn run(&mut self, argv: &[String]) -> Result<String, String> {
        assert_eq!(argv[0], "/usr/sbin/mstpctl");
        let mut r = self.0.borrow_mut();
        let args = argv[1..].to_vec();
        let fails = r.failing.contains(&args[0]);
        r.ran.push(args);
        match fails {
            true => Err("Couldn't change bridge max_age".into()),
            false => Ok(String::new()),
        }
    }
}

impl Fake {
    fn take_ran(&self) -> Vec<String> {
        std::mem::take(&mut self.0.borrow_mut().ran)
            .into_iter()
            .map(|a| a.join(" "))
            .collect()
    }
}

fn mstpd() -> (Mstpd<Fake, Fake>, Fake) {
    let fake = Fake::default();
    let config = MstpdConfig {
        mstpd: "/usr/sbin/mstpd".into(),
        mstpctl: "/usr/sbin/mstpctl".into(),
    };
    (Mstpd::new(config, fake.clone(), fake.clone()), fake)
}

/// Spanning tree on two access ports lan1 and lan2 with `stp` as `/stp`.
fn stp(stp: Value) -> Stp {
    let port = |name: &str| {
        json!({"name": name, "config": {"name": name, "type": "iana-if-type:ethernetCsmacd"},
               "openconfig-if-ethernet:ethernet": {"openconfig-vlan:switched-vlan":
                   {"config": {"interface-mode": "ACCESS", "access-vlan": 1}}}})
    };
    let tree = json!({
        "clixon-switch:vlans": {"vlan": [{"vlan-id": 1, "config": {"vlan-id": 1}}]},
        "openconfig-interfaces:interfaces": {"interface": [port("lan1"), port("lan2")]},
        "openconfig-spanning-tree:stp": stp,
    });
    let ports = ["lan1", "lan2"].map(String::from).into();
    desired_state(&Config::from_json(&tree.to_string()).unwrap(), &ports)
        .unwrap()
        .stp
        .unwrap()
}

fn rstp() -> Stp {
    stp(json!({"global": {"config": {"enabled-protocol": ["RSTP"]}}}))
}

fn configure(mstpd: &mut Mstpd<Fake, Fake>, stp: &Stp, resend: bool) -> Vec<Event> {
    let mut events = Vec::new();
    mstpd
        .configure(Some(stp), "br-lan", "02:aa:bb:cc:dd:0e", resend, &mut |e| {
            events.push(e)
        })
        .unwrap();
    events
}

#[test]
fn rstp_commands() {
    let commands: Vec<String> = commands(&rstp(), "br-lan", "02:aa:bb:cc:dd:0e")
        .into_iter()
        .map(|c| c.join(" "))
        .collect();
    assert_eq!(
        commands,
        [
            "setforcevers br-lan rstp",
            "setfdelay br-lan 30",
            "setmaxage br-lan 20",
            "setfdelay br-lan 15",
            "settxholdcount br-lan 6",
            "setmaxhops br-lan 20",
            "setmstconfid br-lan 0 02AABBCCDD0E",
            "setvid2fid br-lan 0:1-4094",
            "setfid2mstid br-lan 0:0-4095",
            "settreeprio br-lan 0 8",
            "setportpathcost br-lan lan1 0",
            "setportadminedge br-lan lan1 no",
            "setportautoedge br-lan lan1 yes",
            "setportp2p br-lan lan1 auto",
            "setportrestrrole br-lan lan1 no",
            "setbpduguard br-lan lan1 no",
            "setportbpdufilter br-lan lan1 no",
            "setportpathcost br-lan lan2 0",
            "setportadminedge br-lan lan2 no",
            "setportautoedge br-lan lan2 yes",
            "setportp2p br-lan lan2 auto",
            "setportrestrrole br-lan lan2 no",
            "setbpduguard br-lan lan2 no",
            "setportbpdufilter br-lan lan2 no",
            "settreeportprio br-lan lan1 0 8",
            "settreeportcost br-lan lan1 0 0",
            "settreeportprio br-lan lan2 0 8",
            "settreeportcost br-lan lan2 0 0",
        ]
    );
}

#[test]
fn mstp_commands() {
    let mstp = stp(json!({
        "global": {"config": {"enabled-protocol": ["MSTP"]}},
        "mstp": {"config": {"name": "region", "revision": 2, "max-hop": 12,
                            "clixon-switch:bridge-priority": 4096},
                 "mst-instances": {"mst-instance": [
                     {"mst-id": 3, "config": {"mst-id": 3, "vlan": ["10..12", 20], "bridge-priority": 0},
                      "interfaces": {"interface": [
                          {"name": "lan2", "config": {"name": "lan2", "cost": 5, "port-priority": 16}}]}},
                     {"mst-id": 9, "config": {"mst-id": 9}}]}},
        "interfaces": {"interface": [
            {"name": "lan1", "config": {"name": "lan1",
                "edge-port": "openconfig-spanning-tree-types:EDGE_ENABLE", "link-type": "P2P",
                "guard": "ROOT", "bpdu-guard": true, "bpdu-filter": true}}]}
    }));
    let commands: Vec<String> = commands(&mstp, "br-lan", "02:aa:bb:cc:dd:0e")
        .into_iter()
        .map(|c| c.join(" "))
        .collect();
    for expected in [
        "setforcevers br-lan mstp",
        "setmaxhops br-lan 12",
        "setmstconfid br-lan 2 region",
        "createtree br-lan 3",
        "createtree br-lan 9",
        "setvid2fid br-lan 0:1-4094 3:10-12,20",
        "setfid2mstid br-lan 0:0-4095 3:3 9:9",
        "settreeprio br-lan 0 1",
        "settreeprio br-lan 3 0",
        "settreeprio br-lan 9 8",
        "setportadminedge br-lan lan1 yes",
        "setportautoedge br-lan lan1 no",
        "setportp2p br-lan lan1 yes",
        "setportrestrrole br-lan lan1 yes",
        "setbpduguard br-lan lan1 yes",
        "setportbpdufilter br-lan lan1 yes",
        "settreeportprio br-lan lan2 3 1",
        "settreeportcost br-lan lan2 3 5",
        "settreeportprio br-lan lan1 9 8",
    ] {
        assert!(
            commands.contains(&expected.to_string()),
            "{expected}: {commands:#?}"
        );
    }
    let index = |c: &str| commands.iter().position(|x| x == c).unwrap();
    assert!(index("createtree br-lan 3") < index("setfid2mstid br-lan 0:0-4095 3:3 9:9"));
    assert!(index("setfid2mstid br-lan 0:0-4095 3:3 9:9") < index("settreeprio br-lan 3 0"));
}

#[test]
fn lifecycle_and_changed_commands() {
    let (mut mstpd, fake) = mstpd();
    assert!(!mstpd.running());
    assert_eq!(
        mstpd.prepare(true, "br-lan").unwrap(),
        [Event::Started { pid: 1 }]
    );
    assert!(mstpd.running());
    // Waiting for the control socket.
    assert_eq!(fake.take_ran(), ["debuglevel 2"]);

    let events = configure(&mut mstpd, &rstp(), false);
    let ran = fake.take_ran();
    assert_eq!(events.len(), ran.len());
    assert_eq!(ran[0], "addbridge br-lan");
    assert_eq!(ran.len(), 1 + commands(&rstp(), "br-lan", "x").len());

    // Nothing changed: only addbridge, which is idempotent.
    assert!(mstpd.prepare(true, "br-lan").unwrap().is_empty());
    configure(&mut mstpd, &rstp(), false);
    assert_eq!(fake.take_ran(), ["addbridge br-lan"]);

    // One port setting.
    let changed = stp(json!({
        "global": {"config": {"enabled-protocol": ["RSTP"]}},
        "interfaces": {"interface": [{"name": "lan2", "config": {"name": "lan2", "bpdu-filter": true}}]}
    }));
    configure(&mut mstpd, &changed, false);
    assert_eq!(
        fake.take_ran(),
        ["addbridge br-lan", "setportbpdufilter br-lan lan2 yes"]
    );

    // A timer: all timer commands, in their order.
    let timers = stp(json!({
        "global": {"config": {"enabled-protocol": ["RSTP"]}},
        "rstp": {"config": {"max-age": 22, "forwarding-delay": 15}}
    }));
    configure(&mut mstpd, &timers, false);
    assert_eq!(
        fake.take_ran(),
        [
            "addbridge br-lan",
            "setfdelay br-lan 30",
            "setmaxage br-lan 22",
            "setfdelay br-lan 15",
            "setportbpdufilter br-lan lan2 no",
        ]
    );

    // Ports joined: everything again.
    configure(&mut mstpd, &timers, true);
    assert_eq!(fake.take_ran().len(), ran.len());

    // Off: the bridge is released before mstpd stops.
    assert_eq!(
        mstpd.prepare(false, "br-lan").unwrap(),
        [
            Event::Command(vec!["delbridge".into(), "br-lan".into()]),
            Event::Stopped { pid: 1 }
        ]
    );
    assert_eq!(fake.take_ran(), ["delbridge br-lan"]);
    assert!(!mstpd.running());
}

#[test]
fn removed_msti_is_deleted_after_remapping() {
    let (mut mstpd, fake) = mstpd();
    mstpd.prepare(true, "br-lan").unwrap();
    let with_msti = stp(json!({
        "global": {"config": {"enabled-protocol": ["MSTP"]}},
        "mstp": {"mst-instances": {"mst-instance": [
            {"mst-id": 4, "config": {"mst-id": 4, "vlan": [1]}}]}}
    }));
    configure(&mut mstpd, &with_msti, false);
    fake.take_ran();

    let without = stp(json!({"global": {"config": {"enabled-protocol": ["MSTP"]}}}));
    configure(&mut mstpd, &without, false);
    assert_eq!(
        fake.take_ran(),
        [
            "addbridge br-lan",
            "setvid2fid br-lan 0:1-4094",
            "setfid2mstid br-lan 0:0-4095",
            "deletetree br-lan 4",
        ]
    );
}

#[test]
fn exited_mstpd_is_restarted_and_fully_configured() {
    let (mut mstpd, fake) = mstpd();
    mstpd.prepare(true, "br-lan").unwrap();
    fake.take_ran();
    configure(&mut mstpd, &rstp(), false);
    let all = fake.take_ran().len();

    fake.0.borrow_mut().running.clear();
    assert_eq!(
        mstpd.prepare(true, "br-lan").unwrap(),
        [Event::Exited { pid: 1 }, Event::Started { pid: 2 }]
    );
    fake.take_ran();
    configure(&mut mstpd, &rstp(), false);
    assert_eq!(fake.take_ran().len(), all);
}

#[test]
fn failing_command_is_reported_and_retried() {
    let (mut mstpd, fake) = mstpd();
    mstpd.prepare(true, "br-lan").unwrap();
    fake.0.borrow_mut().failing.insert("setmaxage".into());
    let error = mstpd
        .configure(
            Some(&rstp()),
            "br-lan",
            "02:aa:bb:cc:dd:0e",
            false,
            &mut |_| {},
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "mstpctl setmaxage br-lan 20: Couldn't change bridge max_age"
    );
    fake.take_ran();
    fake.0.borrow_mut().failing.clear();
    configure(&mut mstpd, &rstp(), false);
    assert_eq!(
        fake.take_ran().len(),
        1 + commands(&rstp(), "br-lan", "x").len()
    );
}
