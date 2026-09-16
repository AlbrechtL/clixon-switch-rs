//! The DHCP client: busybox udhcpc, one process for the SVI that has
//! `ipv4/config/dhcp-client` set.
//!
//! udhcpc runs in the foreground as a child of clixon_backend and calls a
//! script (scripts/udhcpc-script.sh) on every lease event. The script sets the
//! address with the lease time as its lifetime, which is how the planner tells
//! it from static addresses (see [`crate::ActualState::dhcp_addresses`]), the
//! default route and resolv.conf, and records the lease in `lease.<interface>`
//! for state data.
//!
//! Process handling sits behind [`Processes`], so that the decisions are
//! tested without spawning anything.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use switch_model::{DhcpLease, Ipv4Prefix};

/// How long a stopped udhcpc gets to release its lease and run the script's
/// deconfig, before it is killed.
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhcpConfig {
    /// The udhcpc binary, looked up in PATH unless it contains a '/'.
    pub udhcpc: PathBuf,
    /// The script udhcpc calls on lease events.
    pub script: PathBuf,
    /// Directory for pid and lease files, on tmpfs.
    pub run_dir: PathBuf,
    /// Where the script writes the DNS servers.
    pub resolv_conf: PathBuf,
}

impl DhcpConfig {
    fn pid_file(&self, interface: &str) -> PathBuf {
        self.run_dir.join(format!("udhcpc.{interface}.pid"))
    }

    fn lease_file(&self, interface: &str) -> PathBuf {
        self.run_dir.join(format!("lease.{interface}"))
    }

    /// The udhcpc command line for `interface`: foreground, syslog, release
    /// the lease on exit, and send the host name.
    pub fn argv(&self, interface: &str, hostname: Option<&str>) -> Vec<String> {
        let mut argv: Vec<String> = [
            self.udhcpc.to_string_lossy().as_ref(),
            "-f",
            "-S",
            "-R",
            "-i",
            interface,
            "-s",
            self.script.to_string_lossy().as_ref(),
            "-p",
            self.pid_file(interface).to_string_lossy().as_ref(),
        ]
        .map(String::from)
        .into();
        if let Some(name) = hostname.filter(|n| !n.is_empty()) {
            argv.push("-x".into());
            argv.push(format!("hostname:{name}"));
        }
        argv
    }

    /// Environment for the script.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            (
                "CLIXON_SWITCH_RUNDIR".into(),
                self.run_dir.to_string_lossy().into_owned(),
            ),
            (
                "RESOLV_CONF".into(),
                self.resolv_conf.to_string_lossy().into_owned(),
            ),
        ]
    }
}

/// Starting and stopping processes.
pub trait Processes {
    /// Starts `argv` and returns its pid.
    fn spawn(&mut self, argv: &[String], env: &[(String, String)]) -> io::Result<u32>;
    /// Whether the process started with [`Processes::spawn`] still runs.
    /// Reaps it if not.
    fn running(&mut self, pid: u32) -> bool;
    /// Stops the process: SIGTERM, then SIGKILL after [`STOP_TIMEOUT`].
    fn stop(&mut self, pid: u32);
}

/// Something worth logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Started {
        interface: String,
        pid: u32,
    },
    Stopped {
        interface: String,
        pid: u32,
    },
    /// The client had exited by itself.
    Exited {
        interface: String,
        pid: u32,
    },
}

pub struct DhcpClients<P: Processes> {
    config: DhcpConfig,
    processes: P,
    /// The running client: interface and pid.
    client: Option<(String, u32)>,
}

impl<P: Processes> DhcpClients<P> {
    pub fn new(config: DhcpConfig, processes: P) -> Self {
        DhcpClients {
            config,
            processes,
            client: None,
        }
    }

    pub fn config(&self) -> &DhcpConfig {
        &self.config
    }

    /// The interface whose client runs, if any.
    pub fn interface(&self) -> Option<&str> {
        self.client.as_ref().map(|(i, _)| i.as_str())
    }

    /// Stops the client unless it runs on `wanted` and is still alive.
    /// Called before the kernel is reconciled, so that a client that goes
    /// away releases its lease while its interface still exists.
    pub fn stop_unwanted(&mut self, wanted: Option<&str>) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some((interface, pid)) = self.client.take() {
            if !self.processes.running(pid) {
                events.push(Event::Exited { interface, pid });
            } else if wanted == Some(interface.as_str()) {
                self.client = Some((interface, pid));
            } else {
                self.stop(interface, pid, &mut events);
            }
        }
        events
    }

    /// Makes the client run on `wanted`, or on no interface. `relinked`: the
    /// kernel link of `wanted` was created anew, so a running client is bound
    /// to a link that no longer exists and restarts.
    pub fn sync(
        &mut self,
        wanted: Option<&str>,
        relinked: bool,
        hostname: Option<&str>,
    ) -> io::Result<Vec<Event>> {
        let mut events = self.stop_unwanted(wanted);
        if relinked {
            if let Some((interface, pid)) = self.client.take() {
                self.stop(interface, pid, &mut events);
            }
        }
        if let (Some(interface), None) = (wanted, &self.client) {
            let argv = self.config.argv(interface, hostname);
            let pid = self.processes.spawn(&argv, &self.config.env())?;
            self.client = Some((interface.to_string(), pid));
            events.push(Event::Started {
                interface: interface.to_string(),
                pid,
            });
        }
        Ok(events)
    }

    fn stop(&mut self, interface: String, pid: u32, events: &mut Vec<Event>) {
        self.processes.stop(pid);
        // The script's deconfig removes it; not if udhcpc had to be killed.
        let _ = fs::remove_file(self.config.lease_file(&interface));
        events.push(Event::Stopped { interface, pid });
    }

    /// The lease of the running client, as recorded by the script.
    pub fn lease(&self) -> Option<(String, DhcpLease)> {
        let (interface, _) = self.client.as_ref()?;
        let text = fs::read_to_string(self.config.lease_file(interface)).ok()?;
        Some((interface.clone(), parse_lease(&text)))
    }
}

/// Parses a lease file: `key=value` lines as udhcpc passes them to the
/// script (`ip`, `mask`, `router`, `dns`, `domain`, `serverid`, `lease`),
/// lists separated by spaces. Unknown keys and bad values are skipped.
pub fn parse_lease(text: &str) -> DhcpLease {
    let values: BTreeMap<&str, &str> = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    let addrs = |key: &str| -> Vec<Ipv4Addr> {
        values
            .get(key)
            .map(|v| {
                v.split_whitespace()
                    .filter_map(|a| a.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    let ip = values.get("ip").and_then(|v| v.parse::<Ipv4Addr>().ok());
    let mask = values
        .get("mask")
        .and_then(|v| v.parse::<u8>().ok())
        .filter(|m| *m <= 32);
    DhcpLease {
        address: ip
            .zip(mask)
            .map(|(addr, prefix_len)| Ipv4Prefix { addr, prefix_len }),
        routers: addrs("router"),
        dns_servers: addrs("dns"),
        domain: values
            .get("domain")
            .filter(|d| !d.is_empty())
            .map(|d| d.to_string()),
        server: values.get("serverid").and_then(|v| v.parse().ok()),
        lease_time: values.get("lease").and_then(|v| v.parse().ok()),
        remaining_time: None,
    }
}

/// Stops udhcpc processes left behind by an earlier clixon_backend, found by
/// their pid files in `run_dir`. Returns their pids.
pub fn stop_orphans(run_dir: &Path) -> Vec<u32> {
    let Ok(entries) = fs::read_dir(run_dir) else {
        return Vec::new();
    };
    let mut stopped = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("udhcpc.") && name.ends_with(".pid") {
            let pid = fs::read_to_string(entry.path())
                .ok()
                .and_then(|p| p.trim().parse::<u32>().ok());
            if let Some(pid) = pid.filter(|pid| is_udhcpc(*pid)) {
                terminate(pid, || !Path::new(&format!("/proc/{pid}")).exists());
                stopped.push(pid);
            }
            let _ = fs::remove_file(entry.path());
        } else if name.starts_with("lease.") {
            let _ = fs::remove_file(entry.path());
        }
    }
    stopped
}

fn is_udhcpc(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|comm| comm.trim() == "udhcpc")
}

/// SIGTERM, wait for `gone`, SIGKILL after [`STOP_TIMEOUT`].
fn terminate(pid: u32, mut gone: impl FnMut() -> bool) {
    let Ok(pid_t) = libc::pid_t::try_from(pid) else {
        return;
    };
    unsafe { libc::kill(pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if gone() {
            return;
        }
        sleep(POLL);
    }
    unsafe { libc::kill(pid_t, libc::SIGKILL) };
    let deadline = Instant::now() + STOP_TIMEOUT;
    while !gone() && Instant::now() < deadline {
        sleep(POLL);
    }
}

/// [`Processes`] as children of this process.
#[derive(Default)]
pub struct ChildProcesses {
    children: BTreeMap<u32, Child>,
}

impl Processes for ChildProcesses {
    fn spawn(&mut self, argv: &[String], env: &[(String, String)]) -> io::Result<u32> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
        let child = Command::new(program)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .spawn()?;
        let pid = child.id();
        self.children.insert(pid, child);
        Ok(pid)
    }

    fn running(&mut self, pid: u32) -> bool {
        let Some(child) = self.children.get_mut(&pid) else {
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            // Exited, or reaped by someone else.
            Ok(Some(_)) | Err(_) => {
                self.children.remove(&pid);
                false
            }
        }
    }

    fn stop(&mut self, pid: u32) {
        let Some(mut child) = self.children.remove(&pid) else {
            return;
        };
        terminate(pid, || !matches!(child.try_wait(), Ok(None)));
    }
}
