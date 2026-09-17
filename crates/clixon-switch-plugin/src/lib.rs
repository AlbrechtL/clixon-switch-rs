//! clixon backend plugin that applies the OpenConfig switch configuration to
//! the Linux kernel.
//!
//! Every commit, revert and the startup commit reconcile the kernel with the
//! whole configuration (see `switch_net::reconcile`), rather than applying
//! the difference between two datastore trees. Around that, they start or
//! stop mstpd and configure it (see `switch_net::stp`), and the DHCP client
//! (see `switch_net::dhcp`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clixon_plugin::{
    export_backend_plugin, BackendPlugin, Error, Handle, Level, Result, StateTree, Transaction,
};
use switch_model::{
    parse_loadavg, parse_meminfo, parse_os_release, parse_uptime, state_xml, system_state_xml,
    validate, AddressOrigin, DesiredState, InterfaceState, SystemState,
};
use switch_net::dhcp::{self, ChildProcesses, DhcpClients, DhcpConfig, Event};
use switch_net::netlink::NetlinkBackend;
use switch_net::stp::{self, CommandControl, Mstpd, MstpdConfig};
use switch_net::{reconcile, NetBackend, Op, BRIDGE_NAME};

/// Space-separated names of the links to manage as switch ports, instead of
/// the DSA user ports. For test setups without DSA, e.g. dummy links.
const PORTS_ENV: &str = "CLIXON_SWITCH_PORTS";

/// Set to log the target configuration of every validated commit as JSON.
const LOG_JSON_ENV: &str = "CLIXON_SWITCH_LOG_JSON";

/// The udhcpc binary, instead of "udhcpc" from PATH.
const UDHCPC_ENV: &str = "CLIXON_SWITCH_UDHCPC";

/// The resolv.conf the DHCP client writes, instead of /etc/resolv.conf.
const RESOLV_CONF_ENV: &str = "RESOLV_CONF";

/// Set in a network namespace other than the host's, where the kernel does
/// not leave spanning tree to mstpd (see `NetlinkBackend::stp_in_netns`).
const STP_IN_NETNS_ENV: &str = "CLIXON_SWITCH_STP_IN_NETNS";

/// The mstpd and mstpctl binaries, instead of those in PATH.
const MSTPD_ENV: &str = "CLIXON_SWITCH_MSTPD";
const MSTPCTL_ENV: &str = "CLIXON_SWITCH_MSTPCTL";

struct SwitchPlugin {
    net: NetlinkBackend,
    mstpd: Mstpd<ChildProcesses, CommandControl>,
    dhcp: DhcpClients<ChildProcesses>,
    /// The configuration last applied, for state data.
    applied: DesiredState,
}

/// Where the DHCP client's files are: the script next to the plugin's
/// directory (CLICON_BACKEND_DIR/../udhcpc-script, as installed by the
/// Makefile), pid and lease files with the datastores on tmpfs.
fn dhcp_config(h: Handle) -> Result<DhcpConfig> {
    let option = |name: &str| {
        h.option(name)
            .map(PathBuf::from)
            .ok_or_else(|| Error::msg(format!("clixon option {name} is not set")))
    };
    let backend_dir = option("CLICON_BACKEND_DIR")?;
    Ok(DhcpConfig {
        udhcpc: std::env::var_os(UDHCPC_ENV).map_or("udhcpc".into(), PathBuf::from),
        script: backend_dir
            .parent()
            .unwrap_or(Path::new("/"))
            .join("udhcpc-script"),
        run_dir: option("CLICON_XMLDB_DIR")?,
        resolv_conf: std::env::var_os(RESOLV_CONF_ENV)
            .map_or("/etc/resolv.conf".into(), PathBuf::from),
    })
}

fn log_stp_event(h: Handle, event: stp::Event) {
    let (level, message) = match event {
        stp::Event::Started { pid } => (Level::Info, format!("mstpd started (pid {pid})")),
        stp::Event::Stopped { pid } => (Level::Info, format!("mstpd stopped (pid {pid})")),
        stp::Event::Exited { pid } => (Level::Warning, format!("mstpd (pid {pid}) had exited")),
        stp::Event::Command(args) => (Level::Info, format!("mstpctl {}", args.join(" "))),
    };
    h.log(level, &message);
}

fn log_dhcp_events(h: Handle, events: Vec<Event>) {
    for event in events {
        let (level, message) = match event {
            Event::Started { interface, pid } => (
                Level::Info,
                format!("DHCP client on {interface} started (pid {pid})"),
            ),
            Event::Stopped { interface, pid } => (
                Level::Info,
                format!("DHCP client on {interface} stopped (pid {pid})"),
            ),
            Event::Exited { interface, pid } => (
                Level::Warning,
                format!("DHCP client on {interface} (pid {pid}) had exited"),
            ),
        };
        h.log(level, &message);
    }
}

impl SwitchPlugin {
    fn new(h: Handle) -> Result<Self> {
        let ports: Option<BTreeSet<String>> = std::env::var(PORTS_ENV)
            .ok()
            .map(|v| v.split_whitespace().map(String::from).collect());
        if let Some(ports) = &ports {
            let names: Vec<_> = ports.iter().map(String::as_str).collect();
            h.log(
                Level::Notice,
                &format!("{PORTS_ENV}: switch ports are {}", names.join(" ")),
            );
        }
        let binary = |env: &str, default: &str| {
            std::env::var_os(env).map_or(PathBuf::from(default), PathBuf::from)
        };
        let mstpd_config = MstpdConfig {
            mstpd: binary(MSTPD_ENV, "mstpd"),
            mstpctl: binary(MSTPCTL_ENV, "mstpctl"),
        };
        let mut net = NetlinkBackend::new(ports)?;
        if std::env::var_os(STP_IN_NETNS_ENV).is_some() {
            h.log(
                Level::Notice,
                &format!("{STP_IN_NETNS_ENV}: spanning tree without stp_state, for tests only"),
            );
            net.stp_in_netns();
        }
        Ok(SwitchPlugin {
            net,
            mstpd: Mstpd::new(mstpd_config, ChildProcesses::default(), CommandControl),
            dhcp: DhcpClients::new(dhcp_config(h)?, ChildProcesses::default()),
            applied: DesiredState::default(),
        })
    }

    fn desired(&mut self, json: &str) -> Result<DesiredState> {
        let ports = self.net.observe()?.switch_ports();
        Ok(validate(json, &ports)?)
    }

    fn apply(&mut self, h: Handle, desired: DesiredState) -> Result<()> {
        let wanted = desired.dhcp_svi();
        log_dhcp_events(h, self.dhcp.stop_unwanted(wanted));
        for event in self.mstpd.prepare(desired.stp.is_some(), BRIDGE_NAME)? {
            log_stp_event(h, event);
        }
        let ops = reconcile(&mut self.net, &desired, &mut |op| {
            h.log(Level::Info, &op.to_string())
        })?;
        if desired.stp.is_some() {
            // mstpd forgets the settings of a new bridge and of joining ports.
            let resend = ops.iter().any(|op| {
                matches!(
                    op,
                    Op::CreateBridge { .. }
                        | Op::SetMaster {
                            master: Some(_),
                            ..
                        }
                )
            });
            let bridge_mac = self
                .net
                .interface_states()?
                .remove(BRIDGE_NAME)
                .and_then(|s| s.mac)
                .unwrap_or_default();
            self.mstpd.configure(
                desired.stp.as_ref(),
                BRIDGE_NAME,
                &bridge_mac,
                resend,
                &mut |event| log_stp_event(h, event),
            )?;
        }
        let relinked = ops
            .iter()
            .any(|op| matches!(op, Op::CreateVlan { name, .. } if Some(name.as_str()) == wanted));
        let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").ok();
        let events = self
            .dhcp
            .sync(wanted, relinked, hostname.as_deref().map(str::trim))
            .map_err(|e| Error::msg(format!("cannot start the DHCP client: {e}")))?;
        log_dhcp_events(h, events);
        self.applied = desired;
        Ok(())
    }

    /// Kernel state of every link, with the IPv4 addresses and the DHCP
    /// lease of the SVIs.
    fn interface_states(&mut self) -> Result<BTreeMap<String, InterfaceState>> {
        let mut states = self.net.interface_states()?;
        let actual = self.net.observe()?;
        for name in self.applied.svis.keys() {
            let Some(state) = states.get_mut(name) else {
                continue;
            };
            let static_addrs = actual.addresses.get(name).into_iter().flatten();
            state
                .addresses
                .extend(static_addrs.map(|p| (*p, AddressOrigin::Static)));
            let dhcp_addrs = actual.dhcp_addresses.get(name);
            state.addresses.extend(
                dhcp_addrs
                    .into_iter()
                    .flatten()
                    .map(|(p, _)| (*p, AddressOrigin::Dhcp)),
            );
            if let Some((_, mut lease)) = self.dhcp.lease().filter(|(i, _)| i == name) {
                lease.remaining_time = lease
                    .address
                    .and_then(|a| dhcp_addrs.and_then(|d| d.get(&a)).copied());
                state.dhcp_lease = Some(lease);
            }
        }
        Ok(states)
    }
}

/// Host name, OS release, uptime, load and memory. Files that cannot be
/// read leave their leaves out.
fn system_state() -> SystemState {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let (os_name, os_version) = read("/etc/os-release")
        .or_else(|| read("/usr/lib/os-release"))
        .map(|text| parse_os_release(&text))
        .unwrap_or_default();
    let (memory_total, memory_available) = read("/proc/meminfo")
        .map(|text| parse_meminfo(&text))
        .unwrap_or_default();
    SystemState {
        hostname: read("/proc/sys/kernel/hostname").map(|h| h.trim().to_string()),
        os_name,
        os_version,
        kernel_release: read("/proc/sys/kernel/osrelease").map(|r| r.trim().to_string()),
        uptime: read("/proc/uptime").and_then(|text| parse_uptime(&text)),
        load_average: read("/proc/loadavg").and_then(|text| parse_loadavg(&text)),
        memory_total,
        memory_available,
        current_time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs()),
    }
}

impl BackendPlugin for SwitchPlugin {
    fn start(&mut self, h: Handle) -> Result<()> {
        // Loopback and the DSA conduit come up here already, independent of
        // whether the startup configuration commits.
        let actual = self.net.observe()?;
        for op in actual.platform_ops() {
            h.log(Level::Info, &op.to_string());
            self.net.apply(&op)?;
        }
        // udhcpc and mstpd outlive a clixon_backend that was stopped.
        for pid in dhcp::stop_orphans(&self.dhcp.config().run_dir) {
            h.log(
                Level::Info,
                &format!("stopped DHCP client pid {pid} of an earlier backend"),
            );
        }
        for pid in self.mstpd.stop_orphans(BRIDGE_NAME) {
            h.log(
                Level::Info,
                &format!("stopped mstpd pid {pid} of an earlier backend"),
            );
        }
        let ports = actual.switch_ports();
        if ports.is_empty() {
            h.log(
                Level::Warning,
                &format!("no switch ports found: no DSA user ports, and {PORTS_ENV} is not set"),
            );
        } else {
            let names: Vec<_> = ports.iter().map(String::as_str).collect();
            h.log(Level::Info, &format!("switch ports: {}", names.join(" ")));
        }
        Ok(())
    }

    fn trans_validate(&mut self, h: Handle, tx: &Transaction) -> Result<()> {
        let json = tx.target_json()?;
        if std::env::var_os(LOG_JSON_ENV).is_some() {
            h.log(Level::Notice, &format!("target configuration: {json}"));
        }
        self.desired(&json).map(|_| ())
    }

    fn trans_commit(&mut self, h: Handle, tx: &Transaction) -> Result<()> {
        let desired = self.desired(&tx.target_json()?)?;
        self.apply(h, desired)
    }

    fn trans_revert(&mut self, h: Handle, tx: &Transaction) -> Result<()> {
        let desired = self.desired(&tx.src_json()?)?;
        self.apply(h, desired)
    }

    fn statedata(&mut self, _h: Handle, xpath: Option<&str>, state: &mut StateTree) -> Result<()> {
        // Independent of the configuration, so also before the first commit.
        state.add_xml(&system_state_xml(&system_state()))?;
        if self.applied == DesiredState::default() {
            return Ok(());
        }
        let states = self.interface_states()?;
        // Asking mstpd takes a few mstpctl runs per port: only for requests
        // that may include /stp.
        let wants_stp = xpath.is_none_or(|x| x == "/" || x.contains("stp"));
        let stp_state = match (&self.applied.stp, wants_stp && self.mstpd.running()) {
            (Some(stp), true) => Some(self.mstpd.state(stp, BRIDGE_NAME)),
            _ => None,
        };
        state.add_xml(&state_xml(&self.applied, &states, stp_state.as_ref()))
    }
}

export_backend_plugin!("clixon-switch", SwitchPlugin::new);
