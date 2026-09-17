//! Child processes of clixon_backend: udhcpc, mstpd, snmpd and clixon_snmp.
//!
//! They run in the foreground, so the plugin notices when they exit. They
//! outlive a clixon_backend that is stopped, so a new one stops them by name.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// How long a process gets to exit after SIGTERM, before it is killed.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(20);

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

/// SIGTERM, wait for `gone`, SIGKILL after [`STOP_TIMEOUT`].
pub fn terminate(pid: u32, mut gone: impl FnMut() -> bool) {
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

/// Pids of the processes named `comm` (their /proc/<pid>/comm).
pub fn pids_named(comm: &str) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == comm)
        })
        .collect()
}

/// Whether process `pid` is gone.
pub fn gone(pid: u32) -> bool {
    !Path::new(&format!("/proc/{pid}")).exists()
}
