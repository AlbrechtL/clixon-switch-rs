use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::rc::Rc;

use switch_model::{DhcpLease, Ipv4Prefix};
use switch_net::dhcp::{parse_lease, DhcpClients, DhcpConfig, Event, Processes};

/// Processes that only record what happens to them. Clones share the
/// record, so a test keeps a handle to what the clients own.
#[derive(Default, Clone)]
struct FakeProcesses(Rc<RefCell<Record>>);

#[derive(Default)]
struct Record {
    next_pid: u32,
    running: BTreeSet<u32>,
}

impl Processes for FakeProcesses {
    fn spawn(&mut self, _argv: &[String], _env: &[(String, String)]) -> io::Result<u32> {
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
        assert!(
            self.0.borrow_mut().running.remove(&pid),
            "{pid} not running"
        );
    }
}

fn config() -> DhcpConfig {
    DhcpConfig {
        udhcpc: "udhcpc".into(),
        script: "/usr/lib/clixon-switch/udhcpc-script".into(),
        // Never created: the lease file of a stopped client is removed from
        // here, which must not fail.
        run_dir: PathBuf::from("/nonexistent/clixon-switch"),
        resolv_conf: "/etc/resolv.conf".into(),
    }
}

fn clients() -> (DhcpClients<FakeProcesses>, FakeProcesses) {
    let processes = FakeProcesses::default();
    (DhcpClients::new(config(), processes.clone()), processes)
}

fn started(interface: &str, pid: u32) -> Event {
    Event::Started {
        interface: interface.into(),
        pid,
    }
}

fn stopped(interface: &str, pid: u32) -> Event {
    Event::Stopped {
        interface: interface.into(),
        pid,
    }
}

#[test]
fn command_line() {
    assert_eq!(
        config().argv("vlan1", Some("switch")),
        [
            "udhcpc",
            "-f",
            "-S",
            "-R",
            "-i",
            "vlan1",
            "-s",
            "/usr/lib/clixon-switch/udhcpc-script",
            "-p",
            "/nonexistent/clixon-switch/udhcpc.vlan1.pid",
            "-x",
            "hostname:switch"
        ]
    );
    assert!(!config().argv("vlan1", Some("")).contains(&"-x".to_string()));
    assert!(!config().argv("vlan1", None).contains(&"-x".to_string()));
}

#[test]
fn script_environment() {
    let env = config().env();
    assert!(env.contains(&(
        "CLIXON_SWITCH_RUNDIR".into(),
        "/nonexistent/clixon-switch".into()
    )));
    assert!(env.contains(&("RESOLV_CONF".into(), "/etc/resolv.conf".into())));
}

#[test]
fn start_keep_stop() {
    let (mut dhcp, _) = clients();
    assert_eq!(dhcp.sync(None, false, None).unwrap(), vec![]);

    assert_eq!(
        dhcp.sync(Some("vlan1"), false, None).unwrap(),
        vec![started("vlan1", 1)]
    );
    assert_eq!(dhcp.interface(), Some("vlan1"));

    // Unchanged configuration: the client keeps running.
    assert_eq!(dhcp.stop_unwanted(Some("vlan1")), vec![]);
    assert_eq!(dhcp.sync(Some("vlan1"), false, None).unwrap(), vec![]);

    assert_eq!(dhcp.stop_unwanted(None), vec![stopped("vlan1", 1)]);
    assert_eq!(dhcp.sync(None, false, None).unwrap(), vec![]);
    assert_eq!(dhcp.interface(), None);
}

#[test]
fn moves_to_another_interface() {
    let (mut dhcp, _) = clients();
    dhcp.sync(Some("vlan1"), false, None).unwrap();
    assert_eq!(dhcp.stop_unwanted(Some("mgmt")), vec![stopped("vlan1", 1)]);
    assert_eq!(
        dhcp.sync(Some("mgmt"), false, None).unwrap(),
        vec![started("mgmt", 2)]
    );
}

#[test]
fn restarts_on_a_new_link() {
    let (mut dhcp, _) = clients();
    dhcp.sync(Some("vlan1"), false, None).unwrap();
    assert_eq!(
        dhcp.sync(Some("vlan1"), true, None).unwrap(),
        vec![stopped("vlan1", 1), started("vlan1", 2)]
    );
}

#[test]
fn restarts_after_exiting() {
    let (mut dhcp, processes) = clients();
    dhcp.sync(Some("vlan1"), false, None).unwrap();
    // Killed from outside.
    processes.0.borrow_mut().running.remove(&1);
    assert_eq!(
        dhcp.sync(Some("vlan1"), false, None).unwrap(),
        vec![
            Event::Exited {
                interface: "vlan1".into(),
                pid: 1
            },
            started("vlan1", 2)
        ]
    );
}

#[test]
fn lease_file() {
    let lease = parse_lease(
        "ip=10.99.0.100\nmask=24\nrouter=10.99.0.1 10.99.0.2\ndns=10.99.0.53\ndomain=lab\nserverid=10.99.0.1\nlease=3600\nbogus\n",
    );
    assert_eq!(
        lease,
        DhcpLease {
            address: Some(Ipv4Prefix {
                addr: Ipv4Addr::new(10, 99, 0, 100),
                prefix_len: 24
            }),
            routers: vec![Ipv4Addr::new(10, 99, 0, 1), Ipv4Addr::new(10, 99, 0, 2)],
            dns_servers: vec![Ipv4Addr::new(10, 99, 0, 53)],
            domain: Some("lab".into()),
            server: Some(Ipv4Addr::new(10, 99, 0, 1)),
            lease_time: Some(3600),
            remaining_time: None,
        }
    );
}

#[test]
fn incomplete_lease_file() {
    let lease = parse_lease("ip=10.99.0.100\nmask=33\ndomain=\nlease=soon\n");
    assert_eq!(lease, DhcpLease::default());
}
