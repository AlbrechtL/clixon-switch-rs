//! clixon backend plugin that applies the OpenConfig switch configuration to
//! the Linux kernel.
//!
//! Every commit, revert and the startup commit reconcile the kernel with the
//! whole configuration (see `switch_net::reconcile`), rather than applying
//! the difference between two datastore trees.

use std::collections::BTreeSet;

use clixon_plugin::{
    export_backend_plugin, BackendPlugin, Handle, Level, Result, StateTree, Transaction,
};
use switch_model::{state_xml, validate, DesiredState};
use switch_net::netlink::NetlinkBackend;
use switch_net::{reconcile, NetBackend};

/// Space-separated names of the links to manage as switch ports, instead of
/// the DSA user ports. For test setups without DSA, e.g. dummy links.
const PORTS_ENV: &str = "CLIXON_SWITCH_PORTS";

/// Set to log the target configuration of every validated commit as JSON.
const LOG_JSON_ENV: &str = "CLIXON_SWITCH_LOG_JSON";

struct SwitchPlugin {
    net: NetlinkBackend,
    /// The configuration last applied, for state data.
    applied: DesiredState,
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
        Ok(SwitchPlugin {
            net: NetlinkBackend::new(ports)?,
            applied: DesiredState::default(),
        })
    }

    fn desired(&mut self, json: &str) -> Result<DesiredState> {
        let ports = self.net.observe()?.switch_ports();
        Ok(validate(json, &ports)?)
    }

    fn apply(&mut self, h: Handle, desired: DesiredState) -> Result<()> {
        reconcile(&mut self.net, &desired, &mut |op| {
            h.log(Level::Info, &op.to_string())
        })?;
        self.applied = desired;
        Ok(())
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

    fn statedata(&mut self, _h: Handle, _xpath: Option<&str>, state: &mut StateTree) -> Result<()> {
        if self.applied == DesiredState::default() {
            return Ok(());
        }
        let states = self.net.interface_states()?;
        state.add_xml(&state_xml(&self.applied, &states))
    }
}

export_backend_plugin!("clixon-switch", SwitchPlugin::new);
