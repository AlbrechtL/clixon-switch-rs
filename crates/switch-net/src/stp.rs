//! Spanning tree: mstpd, one process while a protocol is enabled, configured
//! with mstpctl.
//!
//! mstpd runs in the foreground as a child of clixon_backend, like the DHCP
//! client. The kernel leaves spanning tree to it once the bridge has
//! `stp_state` 1 and /sbin/bridge-stp succeeds (see [`crate::Op::SetBridgeStp`]).
//! Every setting is an mstpctl command that sets one value, so the
//! configuration is a list of commands, and a commit runs those that differ
//! from the list last applied. mstpd forgets the settings of a bridge that is
//! created anew and of a port that joins it, so then every command runs again.
//!
//! mstpd, patched in meta-ethernet-switch-os, programs the kernel's per-VLAN
//! spanning tree: the VLAN-to-MSTI mapping and each port's MSTI states.
//!
//! Processes and commands sit behind traits, so that the decisions are tested
//! without mstpd.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::Value;
use switch_model::{
    vlan_ranges, BridgeId, BridgeState, EdgePort, PortState, Stp, StpState, Tree, TreeState,
};

use crate::dhcp::Processes;
use crate::{Error, Result};

/// How long a started mstpd gets to open its control socket.
const START_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MstpdConfig {
    /// The mstpd binary, looked up in PATH unless it contains a '/'.
    pub mstpd: PathBuf,
    pub mstpctl: PathBuf,
}

impl MstpdConfig {
    /// mstpd in the foreground, logging to syslog.
    pub fn mstpd_argv(&self) -> Vec<String> {
        vec![
            self.mstpd.to_string_lossy().into_owned(),
            "-d".into(),
            "-s".into(),
        ]
    }

    fn mstpctl_argv(&self, args: &[String]) -> Vec<String> {
        let mut argv = vec![self.mstpctl.to_string_lossy().into_owned()];
        argv.extend(args.iter().cloned());
        argv
    }
}

/// Running mstpctl.
pub trait Control {
    /// Runs `argv` to completion: its standard output, or an error with its
    /// standard error.
    fn run(&mut self, argv: &[String]) -> std::result::Result<String, String>;
}

/// [`Control`] with child processes.
#[derive(Default)]
pub struct CommandControl;

impl Control for CommandControl {
    fn run(&mut self, argv: &[String]) -> std::result::Result<String, String> {
        let (program, args) = argv.split_first().ok_or("empty command")?;
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| format!("{program}: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            return Ok(stdout);
        }
        // mstpctl prints some errors to standard output.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = [stderr.trim(), stdout.trim()]
            .into_iter()
            .filter(|m| !m.is_empty())
            .collect::<Vec<_>>()
            .join(": ");
        Err(match message.is_empty() {
            true => format!("exit status {}", output.status),
            false => message,
        })
    }
}

/// Something worth logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Started {
        pid: u32,
    },
    Stopped {
        pid: u32,
    },
    /// mstpd had exited by itself.
    Exited {
        pid: u32,
    },
    /// An mstpctl command, before it runs.
    Command(Vec<String>),
}

/// What mstpd was last configured with.
struct Applied {
    stp: Stp,
    commands: Vec<Vec<String>>,
}

pub struct Mstpd<P: Processes, C: Control> {
    config: MstpdConfig,
    processes: P,
    control: C,
    pid: Option<u32>,
    applied: Option<Applied>,
}

impl<P: Processes, C: Control> Mstpd<P, C> {
    pub fn new(config: MstpdConfig, processes: P, control: C) -> Self {
        Mstpd {
            config,
            processes,
            control,
            pid: None,
            applied: None,
        }
    }

    /// Whether mstpd runs.
    pub fn running(&self) -> bool {
        self.pid.is_some()
    }

    /// Before the kernel is reconciled: starts mstpd if spanning tree is
    /// `wanted`, so that it is ready when the bridge hands spanning tree
    /// over. Otherwise stops it, after releasing the bridge, which resets
    /// the kernel's per-VLAN states.
    pub fn prepare(&mut self, wanted: bool, bridge: &str) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        if let Some(pid) = self.pid.filter(|pid| !self.processes.running(*pid)) {
            events.push(Event::Exited { pid });
            self.pid = None;
            self.applied = None;
        }
        match (wanted, self.pid) {
            (true, None) => {
                let argv = self.config.mstpd_argv();
                let pid = self
                    .processes
                    .spawn(&argv, &[])
                    .map_err(|e| Error(format!("cannot start mstpd: {e}")))?;
                self.pid = Some(pid);
                events.push(Event::Started { pid });
                self.wait_ready(pid)?;
            }
            (false, Some(pid)) => {
                let args = strings(&["delbridge", bridge]);
                events.push(Event::Command(args.clone()));
                // mstpd may not know the bridge.
                let _ = self.control.run(&self.config.mstpctl_argv(&args));
                self.processes.stop(pid);
                self.pid = None;
                self.applied = None;
                events.push(Event::Stopped { pid });
            }
            _ => {}
        }
        Ok(events)
    }

    /// Stops mstpd processes left behind by an earlier clixon_backend (every
    /// process named mstpd, since only one can run), after releasing
    /// `bridge` as [`Mstpd::prepare`] does. Returns their pids.
    pub fn stop_orphans(&mut self, bridge: &str) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        let pids: Vec<u32> = entries
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| {
                std::fs::read_to_string(format!("/proc/{pid}/comm"))
                    .is_ok_and(|c| c.trim() == "mstpd")
            })
            .collect();
        if !pids.is_empty() {
            let args = strings(&["delbridge", bridge]);
            let _ = self.control.run(&self.config.mstpctl_argv(&args));
        }
        for pid in &pids {
            crate::dhcp::terminate(*pid, || {
                !std::path::Path::new(&format!("/proc/{pid}")).exists()
            });
        }
        pids
    }

    fn wait_ready(&mut self, pid: u32) -> Result<()> {
        // Sets mstpd's default log level (info), and needs no bridge.
        let argv = self.config.mstpctl_argv(&strings(&["debuglevel", "2"]));
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            match self.control.run(&argv) {
                Ok(_) => return Ok(()),
                Err(e) if Instant::now() >= deadline || !self.processes.running(pid) => {
                    return Err(Error(format!("mstpd did not start: {e}")));
                }
                Err(_) => sleep(POLL),
            }
        }
    }

    /// After the kernel is reconciled: configures mstpd with `wanted`.
    /// `resend`: the bridge was created anew or ports joined it, so mstpd
    /// has lost settings. `bridge_mac` names the MST region by default.
    pub fn configure(
        &mut self,
        wanted: Option<&Stp>,
        bridge: &str,
        bridge_mac: &str,
        resend: bool,
        log: &mut dyn FnMut(Event),
    ) -> Result<()> {
        let Some(stp) = wanted else {
            return Ok(());
        };
        if resend {
            self.applied = None;
        }
        let previous = self.applied.take();
        let next = commands(stp, bridge, bridge_mac);
        let mut run = vec![strings(&["addbridge", bridge])];
        run.extend(changed_commands(previous.as_ref(), &next, stp, bridge));
        for args in run {
            log(Event::Command(args.clone()));
            self.control
                .run(&self.config.mstpctl_argv(&args))
                .map_err(|e| Error(format!("mstpctl {}: {e}", args.join(" "))))?;
        }
        self.applied = Some(Applied {
            stp: stp.clone(),
            commands: next,
        });
        Ok(())
    }

    /// What mstpd reports about the spanning trees of `stp`. Missing parts
    /// are left out.
    pub fn state(&mut self, stp: &Stp, bridge: &str) -> StpState {
        let mut state = StpState {
            cist: self.tree_state(bridge, None, &stp.cist),
            mstis: BTreeMap::new(),
        };
        for (id, msti) in &stp.mstis {
            state
                .mstis
                .insert(*id, self.tree_state(bridge, Some(*id), &msti.tree));
        }
        state
    }

    fn json(&mut self, args: &[&str]) -> Option<Value> {
        let mut all = strings(&["-f", "json"]);
        all.extend(strings(args));
        let output = self.control.run(&self.config.mstpctl_argv(&all)).ok()?;
        // An array with one entry per bridge or port asked for.
        match serde_json::from_str(output.trim()).ok()? {
            Value::Array(mut entries) if !entries.is_empty() => Some(entries.swap_remove(0)),
            Value::Array(_) => None,
            object => Some(object),
        }
    }

    fn tree_state(&mut self, bridge: &str, msti: Option<u16>, tree: &Tree) -> TreeState {
        let id = msti.map(|m| m.to_string());
        let bridge_json = match &id {
            None => self.json(&["showbridge", bridge]),
            Some(id) => self.json(&["showtree", bridge, id]),
        };
        let mut state = TreeState {
            bridge: bridge_json
                .as_ref()
                .map(|j| bridge_state(j, msti.is_some())),
            ports: BTreeMap::new(),
        };
        for port in tree.ports.keys() {
            let port_json = match &id {
                None => self.json(&["showportdetail", bridge, port]),
                Some(id) => self.json(&["showtreeport", bridge, port, id]),
            };
            if let Some(j) = port_json {
                state
                    .ports
                    .insert(port.clone(), port_state(&j, msti.is_some()));
            }
        }
        state
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

/// The largest forward delay, which allows any max age.
const MAX_FORWARD_DELAY: &str = "30";

/// The mstpctl commands (without "mstpctl") that configure `bridge` with
/// `stp`, each setting one value.
pub fn commands(stp: &Stp, bridge: &str, bridge_mac: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut add = |args: &[&str]| commands.push(strings(args));
    let br = bridge;

    add(&["setforcevers", br, stp.protocol.mstpd_name()]);
    // mstpd checks 2 * (forward delay - 1) >= max age on each change, so
    // whatever the timers were, the longest forward delay goes first.
    add(&["setfdelay", br, MAX_FORWARD_DELAY]);
    add(&["setmaxage", br, &stp.max_age.to_string()]);
    add(&["setfdelay", br, &stp.forward_delay.to_string()]);
    add(&["settxholdcount", br, &stp.hold_count.to_string()]);
    add(&["setmaxhops", br, &stp.max_hops.to_string()]);
    let default_name = bridge_mac.replace(':', "").to_uppercase();
    add(&[
        "setmstconfid",
        br,
        &stp.region_revision.to_string(),
        stp.region_name.as_deref().unwrap_or(&default_name),
    ]);

    // VLAN -> FID -> MSTI, with FID = MSTID. The full tables, so that VLANs
    // no longer in an MSTI return to the CIST.
    for id in stp.mstis.keys() {
        add(&["createtree", br, &id.to_string()]);
    }
    let mut vid2fid = vec!["setvid2fid".to_string(), br.to_string(), "0:1-4094".into()];
    let mut fid2mstid = vec![
        "setfid2mstid".to_string(),
        br.to_string(),
        "0:0-4095".into(),
    ];
    for (id, msti) in &stp.mstis {
        if !msti.vlans.is_empty() {
            let list: Vec<String> = vlan_ranges(&msti.vlans)
                .iter()
                .map(|r| r.replace("..", "-"))
                .collect();
            vid2fid.push(format!("{id}:{}", list.join(",")));
        }
        fid2mstid.push(format!("{id}:{id}"));
    }
    commands.push(vid2fid);
    commands.push(fid2mstid);

    let mut trees: Vec<(u16, &Tree)> = vec![(0, &stp.cist)];
    trees.extend(stp.mstis.iter().map(|(id, m)| (*id, &m.tree)));
    for (id, tree) in &trees {
        commands.push(strings(&[
            "settreeprio",
            br,
            &id.to_string(),
            &(tree.bridge_priority / 4096).to_string(),
        ]));
    }

    for (port, f) in &stp.ports {
        let cist = stp.cist.ports.get(port).copied().unwrap_or_default();
        let cost = cist.cost.unwrap_or(0).to_string();
        let mut add = |args: &[&str]| commands.push(strings(args));
        add(&["setportpathcost", br, port, &cost]);
        add(&[
            "setportadminedge",
            br,
            port,
            yes_no(f.edge == EdgePort::Enable),
        ]);
        add(&[
            "setportautoedge",
            br,
            port,
            yes_no(f.edge == EdgePort::Auto),
        ]);
        add(&[
            "setportp2p",
            br,
            port,
            f.point_to_point.map_or("auto", yes_no),
        ]);
        add(&["setportrestrrole", br, port, yes_no(f.root_guard)]);
        add(&["setbpduguard", br, port, yes_no(f.bpdu_guard)]);
        add(&["setportbpdufilter", br, port, yes_no(f.bpdu_filter)]);
    }
    for (id, tree) in &trees {
        for (port, p) in &tree.ports {
            let id = id.to_string();
            commands.push(strings(&[
                "settreeportprio",
                br,
                port,
                &id,
                &(p.priority / 16).to_string(),
            ]));
            commands.push(strings(&[
                "settreeportcost",
                br,
                port,
                &id,
                &p.cost.unwrap_or(0).to_string(),
            ]));
        }
    }
    commands
}

/// The commands of `next` that `previous` did not run, in order, with
/// `deletetree` for MSTIs that are gone once their VLANs are mapped away.
/// The timer commands run together, as their order matters.
fn changed_commands(
    previous: Option<&Applied>,
    next: &[Vec<String>],
    stp: &Stp,
    bridge: &str,
) -> Vec<Vec<String>> {
    let ran: BTreeSet<&Vec<String>> = previous.into_iter().flat_map(|a| &a.commands).collect();
    let gone: Vec<u16> = previous
        .map(|a| {
            a.stp
                .mstis
                .keys()
                .filter(|id| !stp.mstis.contains_key(id))
                .copied()
                .collect()
        })
        .unwrap_or_default();

    let is_timer = |c: &Vec<String>| c[0] == "setfdelay" || c[0] == "setmaxage";
    let timers_changed = next
        .iter()
        .filter(|c| is_timer(c))
        .any(|c| !ran.contains(c));

    let mut out = Vec::new();
    for command in next {
        if !ran.contains(command) || (timers_changed && is_timer(command)) {
            out.push(command.clone());
        }
        if command[0] == "setfid2mstid" {
            for id in &gone {
                out.push(strings(&["deletetree", bridge, &id.to_string()]));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// State, from mstpctl's JSON
// ---------------------------------------------------------------------------

fn text<'a>(json: &'a Value, key: &str) -> Option<&'a str> {
    json.get(key).and_then(Value::as_str)
}

fn number<T: std::str::FromStr>(json: &Value, key: &str) -> Option<T> {
    text(json, key).and_then(|v| v.parse().ok())
}

/// "8.000.AA:BB:CC:DD:EE:FF": priority / 4096, system ID extension, MAC.
fn bridge_id(json: &Value, key: &str) -> Option<BridgeId> {
    let mut parts = text(json, key)?.splitn(3, '.');
    let priority = u16::from_str_radix(parts.next()?, 16).ok()?;
    let _system_id = parts.next()?;
    let address = parts.next()?.to_lowercase();
    Some(BridgeId {
        priority: priority.checked_mul(4096)?,
        address,
    })
}

/// "8.001": priority / 16, port number.
fn port_id(json: &Value, key: &str) -> Option<(u8, u16)> {
    let (priority, number) = text(json, key)?.split_once('.')?;
    Some((
        u8::from_str_radix(priority, 16).ok()?.checked_mul(16)?,
        u16::from_str_radix(number, 16).ok()?,
    ))
}

/// `showbridge` (CIST) or `showtree` (MSTI).
fn bridge_state(json: &Value, msti: bool) -> BridgeState {
    // "lan3 (#3)", empty or "none" on the root bridge.
    let root_port = text(json, "root-port")
        .and_then(|p| p.split(" (#").next())
        .filter(|p| !p.is_empty() && *p != "none")
        .map(String::from);
    let (root, cost) = match msti {
        false => ("designated-root", "path-cost"),
        true => ("regional-root", "internal-path-cost"),
    };
    BridgeState {
        bridge: bridge_id(json, "bridge-id"),
        root: bridge_id(json, root),
        root_port,
        root_cost: number(json, cost),
        topology_changes: number(json, "topology-change-count"),
    }
}

/// `showportdetail` (CIST) or `showtreeport` (MSTI).
fn port_state(json: &Value, msti: bool) -> PortState {
    let role = match text(json, "role") {
        Some("Root") => Some("ROOT"),
        Some("Designated") => Some("DESIGNATED"),
        Some("Alternate") => Some("ALTERNATE"),
        Some("Backup") => Some("BACKUP"),
        _ => None,
    };
    // mstpd reports RSTP states. OpenConfig's DISABLED is a port that does
    // not take part; mstpd reports discarding for it too.
    let enabled = msti || text(json, "enabled") != Some("no");
    let port_state = match (enabled, text(json, "state")) {
        (false, _) => Some("DISABLED"),
        (true, Some("discarding")) => Some("BLOCKING"),
        (true, Some("learning")) => Some("LEARNING"),
        (true, Some("forwarding")) => Some("FORWARDING"),
        _ => None,
    };
    let designated_port = port_id(json, "designated-port");
    let (root, cost) = match msti {
        false => ("designated-root", "dsgn-external-cost"),
        true => ("dsgn-regional-root", "dsgn-internal-cost"),
    };
    PortState {
        port_num: port_id(json, "port-id").map(|(_, n)| n),
        role,
        port_state,
        designated_root: bridge_id(json, root),
        designated_cost: number(json, cost),
        designated_bridge: bridge_id(json, "designated-bridge"),
        designated_port_priority: designated_port.map(|(p, _)| p),
        designated_port_num: designated_port.map(|(_, n)| n),
        forward_transitions: number(json, "num-transition-fwd"),
        bpdu_sent: number(json, "num-tx-bpdu"),
        bpdu_received: number(json, "num-rx-bpdu"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_and_port_ids() {
        let json: Value = serde_json::from_str(
            r#"{"bridge-id": "8.000.AA:BB:CC:DD:EE:0F", "designated-root": "1.000.02:00:00:00:00:01",
                "root-port": "lan3 (#3)", "path-cost": "20000", "topology-change-count": "4"}"#,
        )
        .unwrap();
        let state = bridge_state(&json, false);
        assert_eq!(
            state.bridge,
            Some(BridgeId {
                priority: 32768,
                address: "aa:bb:cc:dd:ee:0f".into()
            })
        );
        assert_eq!(state.root.unwrap().priority, 4096);
        assert_eq!(state.root_port.as_deref(), Some("lan3"));
        assert_eq!(state.root_cost, Some(20000));
        assert_eq!(state.topology_changes, Some(4));

        let port: Value = serde_json::from_str(
            r#"{"enabled": "yes", "role": "Alternate", "port-id": "8.002", "state": "discarding",
                "designated-port": "F.00A", "num-tx-bpdu": "7"}"#,
        )
        .unwrap();
        let state = port_state(&port, false);
        assert_eq!(state.role, Some("ALTERNATE"));
        assert_eq!(state.port_state, Some("BLOCKING"));
        assert_eq!(state.port_num, Some(2));
        assert_eq!(state.designated_port_priority, Some(240));
        assert_eq!(state.designated_port_num, Some(10));
        assert_eq!(state.bpdu_sent, Some(7));
    }
}
