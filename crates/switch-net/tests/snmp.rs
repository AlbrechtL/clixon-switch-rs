use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;

use switch_net::process::Processes;
use switch_net::snmp::{Event, SnmpConfig, Snmpd, CLIXON_SNMP, SNMPD};

/// Processes that only record what happens. A started snmpd creates its
/// AgentX socket, unless `broken`. Clones share the record.
#[derive(Clone)]
struct Fake(Rc<RefCell<Record>>);

struct Record {
    socket: PathBuf,
    next_pid: u32,
    /// pid -> binary name.
    running: Vec<(u32, String)>,
    broken: bool,
}

impl Processes for Fake {
    fn spawn(&mut self, argv: &[String], _env: &[(String, String)]) -> io::Result<u32> {
        let mut r = self.0.borrow_mut();
        r.next_pid += 1;
        let pid = r.next_pid;
        let name = PathBuf::from(&argv[0])
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if name == SNMPD {
            if r.broken {
                return Ok(pid);
            }
            fs::write(&r.socket, "").unwrap();
        }
        r.running.push((pid, name));
        Ok(pid)
    }

    fn running(&mut self, pid: u32) -> bool {
        self.0.borrow().running.iter().any(|(p, _)| *p == pid)
    }

    fn stop(&mut self, pid: u32) {
        let mut r = self.0.borrow_mut();
        let before = r.running.len();
        r.running.retain(|(p, _)| *p != pid);
        assert_eq!(r.running.len(), before - 1, "pid {pid} was not running");
    }
}

impl Fake {
    fn names(&self) -> BTreeSet<String> {
        self.0
            .borrow()
            .running
            .iter()
            .map(|(_, n)| n.clone())
            .collect()
    }

    /// Lets the process named `name` exit by itself.
    fn exit(&self, name: &str) {
        self.0.borrow_mut().running.retain(|(_, n)| n != name);
    }
}

struct Setup {
    dir: PathBuf,
    snmp: Snmpd<Fake>,
    fake: Fake,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn setup(test: &str) -> Setup {
    let dir = std::env::temp_dir().join(format!("switch-net-snmp-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let config = SnmpConfig {
        snmpd: "/usr/sbin/snmpd".into(),
        clixon_snmp: "/usr/sbin/clixon_snmp".into(),
        clixon_config: "/etc/clixon.xml".into(),
        conf_file: dir.join("snmpd.conf"),
        persistent_dir: dir.join("persistent"),
        agentx_socket: dir.join("agentx.sock"),
    };
    let fake = Fake(Rc::new(RefCell::new(Record {
        socket: config.agentx_socket.clone(),
        next_pid: 0,
        running: Vec::new(),
        broken: false,
    })));
    Setup {
        dir,
        snmp: Snmpd::new(config, fake.clone()),
        fake,
    }
}

fn started(name: &'static str, pid: u32) -> Event {
    Event::Started { name, pid }
}

fn stopped(name: &'static str, pid: u32) -> Event {
    Event::Stopped { name, pid }
}

#[test]
fn command_lines() {
    let s = setup("argv");
    let config = s.snmp.config();
    assert_eq!(
        config.snmpd_argv(),
        [
            "/usr/sbin/snmpd".to_string(),
            "-f".into(),
            "-Lsd".into(),
            "-C".into(),
            "-c".into(),
            s.dir.join("snmpd.conf").to_string_lossy().into_owned(),
            format!("--persistentDir={}", s.dir.join("persistent").display()),
        ]
    );
    assert_eq!(
        config.clixon_snmp_argv(),
        ["/usr/sbin/clixon_snmp", "-f", "/etc/clixon.xml", "-l", "s"]
    );
}

#[test]
fn start_keep_restart_stop() {
    let mut s = setup("lifecycle");
    assert_eq!(s.snmp.sync(None).unwrap(), []);

    let events = s.snmp.sync(Some("agentaddress udp:0.0.0.0:161\n")).unwrap();
    assert_eq!(events, [started(SNMPD, 1), started(CLIXON_SNMP, 2)]);
    assert!(s.snmp.running());
    let conf = s.dir.join("snmpd.conf");
    assert_eq!(
        fs::read_to_string(&conf).unwrap(),
        "agentaddress udp:0.0.0.0:161\n"
    );
    assert_eq!(
        fs::metadata(&conf).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // The same configuration: nothing to do.
    assert_eq!(
        s.snmp.sync(Some("agentaddress udp:0.0.0.0:161\n")).unwrap(),
        []
    );

    // Another one: both restart, the subagent stops first.
    let events = s
        .snmp
        .sync(Some("agentaddress udp:0.0.0.0:1161\n"))
        .unwrap();
    assert_eq!(
        events,
        [
            stopped(CLIXON_SNMP, 2),
            stopped(SNMPD, 1),
            started(SNMPD, 3),
            started(CLIXON_SNMP, 4)
        ]
    );

    let events = s.snmp.sync(None).unwrap();
    assert_eq!(events, [stopped(CLIXON_SNMP, 4), stopped(SNMPD, 3)]);
    assert!(!s.snmp.running());
    assert!(s.fake.names().is_empty());
}

#[test]
fn exited_processes_come_back() {
    let mut s = setup("exited");
    let conf = Some("master agentx\n");
    s.snmp.sync(conf).unwrap();

    s.fake.exit(CLIXON_SNMP);
    let events = s.snmp.sync(conf).unwrap();
    assert_eq!(
        events,
        [
            Event::Exited {
                name: CLIXON_SNMP,
                pid: 2
            },
            started(CLIXON_SNMP, 3)
        ]
    );

    // Without snmpd, clixon_snmp has nothing to talk to: both restart.
    s.fake.exit(SNMPD);
    let events = s.snmp.sync(conf).unwrap();
    assert_eq!(
        events,
        [
            Event::Exited {
                name: SNMPD,
                pid: 1
            },
            stopped(CLIXON_SNMP, 3),
            started(SNMPD, 4),
            started(CLIXON_SNMP, 5)
        ]
    );
    assert_eq!(
        s.fake.names(),
        BTreeSet::from([SNMPD.to_string(), CLIXON_SNMP.to_string()])
    );
}

#[test]
fn snmpd_that_does_not_start() {
    let mut s = setup("broken");
    s.fake.0.borrow_mut().broken = true;
    let error = s.snmp.sync(Some("bogus\n")).unwrap_err();
    assert!(error.0.contains("snmpd exited at start"), "{error}");
    assert!(!s.snmp.running());
}

#[test]
fn saved_users_are_removed_before_start() {
    let mut s = setup("persistent");
    let persistent = s.dir.join("persistent");
    fs::create_dir_all(&persistent).unwrap();
    fs::write(
        persistent.join("snmpd.conf"),
        "usmUser 1 3 0x80001f88 \"old\" \"old\" NULL .1.3.6.1.6.3.10.1.1.3 0x01 .1.3.6.1.6.3.10.1.2.1 \"\" \"\"\nengineBoots 7\n",
    )
    .unwrap();
    s.snmp.sync(Some("master agentx\n")).unwrap();
    assert_eq!(
        fs::read_to_string(persistent.join("snmpd.conf")).unwrap(),
        "engineBoots 7\n"
    );
}
