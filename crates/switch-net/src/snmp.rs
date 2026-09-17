//! The SNMP agent: net-snmp's snmpd, and clixon_snmp as its AgentX subagent
//! for the bridge MIBs, while `/snmp` enables the engine.
//!
//! Both run in the foreground as children of clixon_backend. snmpd reads a
//! configuration file the plugin writes (see [`switch_model::snmpd_conf`]),
//! and restarts, with clixon_snmp, whenever that changes. clixon_snmp
//! connects to snmpd's AgentX socket and, like any clixon client, to
//! clixon_backend; it starts once the socket exists. It asks the backend
//! only after the commit that started it has returned.
//!
//! Processes sit behind [`Processes`], so that the decisions are tested
//! without spawning anything.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use switch_model::strip_persistent_users;

use crate::process::{self, Processes};
use crate::{Error, Result};

/// How long a started snmpd gets to open its AgentX socket.
const START_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);

pub const SNMPD: &str = "snmpd";
pub const CLIXON_SNMP: &str = "clixon_snmp";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnmpConfig {
    /// The snmpd binary, looked up in PATH unless it contains a '/'.
    pub snmpd: PathBuf,
    pub clixon_snmp: PathBuf,
    /// clixon.xml, for clixon_snmp.
    pub clixon_config: PathBuf,
    /// snmpd.conf, written by the plugin: on tmpfs, it holds the USM keys.
    pub conf_file: PathBuf,
    /// snmpd's persistent directory, on flash, so that engineBoots keeps
    /// counting across reboots, as SNMPv3 requires.
    pub persistent_dir: PathBuf,
    /// snmpd's AgentX socket (clixon's CLICON_SNMP_AGENT_SOCK).
    pub agentx_socket: PathBuf,
}

impl SnmpConfig {
    /// snmpd in the foreground, logging to syslog, with no configuration but
    /// `conf_file`.
    pub fn snmpd_argv(&self) -> Vec<String> {
        vec![
            self.snmpd.to_string_lossy().into_owned(),
            "-f".into(),
            "-Lsd".into(),
            "-C".into(),
            "-c".into(),
            self.conf_file.to_string_lossy().into_owned(),
            format!("--persistentDir={}", self.persistent_dir.to_string_lossy()),
        ]
    }

    /// clixon_snmp, logging to syslog.
    pub fn clixon_snmp_argv(&self) -> Vec<String> {
        vec![
            self.clixon_snmp.to_string_lossy().into_owned(),
            "-f".into(),
            self.clixon_config.to_string_lossy().into_owned(),
            "-l".into(),
            "s".into(),
        ]
    }

    fn persistent_file(&self) -> PathBuf {
        self.persistent_dir.join("snmpd.conf")
    }
}

/// Something worth logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Started {
        name: &'static str,
        pid: u32,
    },
    Stopped {
        name: &'static str,
        pid: u32,
    },
    /// The process had exited by itself.
    Exited {
        name: &'static str,
        pid: u32,
    },
}

pub struct Snmpd<P: Processes> {
    config: SnmpConfig,
    processes: P,
    snmpd: Option<u32>,
    subagent: Option<u32>,
    /// The configuration snmpd runs with.
    conf: Option<String>,
}

impl<P: Processes> Snmpd<P> {
    pub fn new(config: SnmpConfig, processes: P) -> Self {
        Snmpd {
            config,
            processes,
            snmpd: None,
            subagent: None,
            conf: None,
        }
    }

    pub fn config(&self) -> &SnmpConfig {
        &self.config
    }

    /// Whether snmpd runs.
    pub fn running(&self) -> bool {
        self.snmpd.is_some()
    }

    /// Makes snmpd run with `conf`, and clixon_snmp with it, or neither if
    /// `conf` is None.
    pub fn sync(&mut self, conf: Option<&str>) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        for (slot, name) in [(&mut self.snmpd, SNMPD), (&mut self.subagent, CLIXON_SNMP)] {
            if let Some(pid) = slot.filter(|pid| !self.processes.running(*pid)) {
                events.push(Event::Exited { name, pid });
                *slot = None;
            }
        }
        let restart = conf.is_some_and(|c| self.snmpd.is_none() || self.conf.as_deref() != Some(c));
        if conf.is_none() || restart {
            self.stop(&mut events);
        }
        let Some(conf) = conf else {
            return Ok(events);
        };
        if restart {
            self.start_snmpd(conf, &mut events)?;
        }
        if self.subagent.is_none() {
            let argv = self.config.clixon_snmp_argv();
            let pid = self
                .processes
                .spawn(&argv, &[])
                .map_err(|e| Error(format!("cannot start {CLIXON_SNMP}: {e}")))?;
            self.subagent = Some(pid);
            events.push(Event::Started {
                name: CLIXON_SNMP,
                pid,
            });
        }
        Ok(events)
    }

    fn stop(&mut self, events: &mut Vec<Event>) {
        // The subagent first: it would log snmpd going away.
        for (slot, name) in [(&mut self.subagent, CLIXON_SNMP), (&mut self.snmpd, SNMPD)] {
            if let Some(pid) = slot.take() {
                self.processes.stop(pid);
                events.push(Event::Stopped { name, pid });
            }
        }
        self.conf = None;
    }

    fn start_snmpd(&mut self, conf: &str, events: &mut Vec<Event>) -> Result<()> {
        let context = |what: &str, path: &Path| {
            let what = what.to_string();
            let path = path.display().to_string();
            move |e: io::Error| Error(format!("{what} {path}: {e}"))
        };
        write_private(&self.config.conf_file, conf)
            .map_err(context("cannot write", &self.config.conf_file))?;
        fs::create_dir_all(&self.config.persistent_dir)
            .map_err(context("cannot create", &self.config.persistent_dir))?;
        let persistent = self.config.persistent_file();
        if let Ok(text) = fs::read_to_string(&persistent) {
            write_private(&persistent, &strip_persistent_users(&text))
                .map_err(context("cannot write", &persistent))?;
        }
        let socket = &self.config.agentx_socket;
        let _ = fs::remove_file(socket);

        let argv = self.config.snmpd_argv();
        let pid = self
            .processes
            .spawn(&argv, &[])
            .map_err(|e| Error(format!("cannot start {SNMPD}: {e}")))?;
        self.snmpd = Some(pid);
        self.conf = Some(conf.to_string());
        events.push(Event::Started { name: SNMPD, pid });

        let deadline = Instant::now() + START_TIMEOUT;
        while !socket.exists() {
            if !self.processes.running(pid) {
                self.snmpd = None;
                self.conf = None;
                return Err(Error(format!(
                    "{SNMPD} exited at start: see its log for errors in {}",
                    self.config.conf_file.display()
                )));
            }
            if Instant::now() >= deadline {
                return Err(Error(format!(
                    "{SNMPD} did not open its AgentX socket {}",
                    socket.display()
                )));
            }
            sleep(POLL);
        }
        Ok(())
    }

    /// Stops snmpd and clixon_snmp processes left behind by an earlier
    /// clixon_backend. Returns their names and pids.
    pub fn stop_orphans(&mut self) -> Vec<(&'static str, u32)> {
        let mut stopped = Vec::new();
        for name in [CLIXON_SNMP, SNMPD] {
            for pid in process::pids_named(name) {
                process::terminate(pid, || process::gone(pid));
                stopped.push((name, pid));
            }
        }
        stopped
    }
}

/// Writes `text` to `path` through a temporary file, readable by the owner
/// only.
fn write_private(path: &Path, text: &str) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    // A leftover would keep its mode.
    let _ = fs::remove_file(&tmp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}
