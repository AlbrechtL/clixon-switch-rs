//! Kernel side of the switch.
//!
//! [`ActualState`] is what the kernel looks like, [`plan`] computes the
//! [`Op`]s from there to a [`DesiredState`], and a [`NetBackend`] observes
//! and applies. [`reconcile`] ties them together and repeats until the
//! kernel matches, because some operations have side effects that are simpler
//! to observe than to predict (e.g. enabling VLAN filtering on a bridge that
//! already has ports).

pub mod dhcp;
pub mod fake;
pub mod netlink;
mod plan;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub use plan::plan;
pub use switch_model::{DesiredState, Ipv4Prefix, BRIDGE_NAME};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkKind {
    /// DSA user port, i.e. a front port of the switch.
    Dsa {
        conduit: Option<String>,
    },
    Bridge {
        vlan_filtering: bool,
        default_pvid: u16,
    },
    Vlan {
        parent: String,
        id: u16,
    },
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub kind: LinkKind,
    /// Administrative state (IFF_UP).
    pub up: bool,
    pub master: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VlanFlags {
    pub pvid: bool,
    pub untagged: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActualState {
    pub links: BTreeMap<String, Link>,
    /// Bridge VLAN entries by device: bridge ports, and the bridge itself for
    /// its "self" entries.
    pub bridge_vlans: BTreeMap<String, BTreeMap<u16, VlanFlags>>,
    /// Permanent IPv4 addresses by device: the static ones.
    pub addresses: BTreeMap<String, BTreeSet<Ipv4Prefix>>,
    /// IPv4 addresses with a limited lifetime by device, with their remaining
    /// valid lifetime in seconds. Only the DHCP client's script adds these.
    pub dhcp_addresses: BTreeMap<String, BTreeMap<Ipv4Prefix, u32>>,
}

impl ActualState {
    /// The front ports of the switch.
    pub fn switch_ports(&self) -> BTreeSet<String> {
        self.links
            .iter()
            .filter(|(_, l)| matches!(l.kind, LinkKind::Dsa { .. }))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Operations that do not depend on the configuration: loopback up, and
    /// the DSA conduits up, without which no port passes traffic.
    pub fn platform_ops(&self) -> Vec<Op> {
        let mut wanted_up: BTreeSet<&str> = BTreeSet::from(["lo"]);
        for link in self.links.values() {
            if let LinkKind::Dsa {
                conduit: Some(conduit),
            } = &link.kind
            {
                wanted_up.insert(conduit);
            }
        }
        wanted_up
            .into_iter()
            .filter(|name| self.links.get(*name).is_some_and(|l| !l.up))
            .map(|name| Op::SetUp {
                name: name.into(),
                up: true,
            })
            .collect()
    }
}

/// One kernel change. Display gives the iproute2 equivalent, for logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// A bridge with vlan_filtering 1 and vlan_default_pvid 0, down.
    CreateBridge {
        name: String,
    },
    /// Sets vlan_filtering 1 and vlan_default_pvid 0 on an existing bridge.
    ConfigureBridge {
        name: String,
    },
    DeleteLink {
        name: String,
    },
    /// An 802.1Q link, down.
    CreateVlan {
        name: String,
        parent: String,
        id: u16,
    },
    SetMaster {
        name: String,
        master: Option<String>,
    },
    SetUp {
        name: String,
        up: bool,
    },
    /// Adds the entry, or updates its flags. On the bridge device itself this
    /// is a "self" entry.
    AddBridgeVlan {
        dev: String,
        vid: u16,
        flags: VlanFlags,
    },
    DelBridgeVlan {
        dev: String,
        vid: u16,
    },
    AddAddress {
        dev: String,
        prefix: Ipv4Prefix,
    },
    DelAddress {
        dev: String,
        prefix: Ipv4Prefix,
    },
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let self_flag = |dev: &str| if dev == BRIDGE_NAME { " self" } else { "" };
        match self {
            Op::CreateBridge { name } => write!(
                f,
                "ip link add {name} type bridge vlan_filtering 1 vlan_default_pvid 0"
            ),
            Op::ConfigureBridge { name } => write!(
                f,
                "ip link set {name} type bridge vlan_filtering 1 vlan_default_pvid 0"
            ),
            Op::DeleteLink { name } => write!(f, "ip link del {name}"),
            Op::CreateVlan { name, parent, id } => {
                write!(f, "ip link add link {parent} name {name} type vlan id {id}")
            }
            Op::SetMaster {
                name,
                master: Some(m),
            } => write!(f, "ip link set {name} master {m}"),
            Op::SetMaster { name, master: None } => write!(f, "ip link set {name} nomaster"),
            Op::SetUp { name, up } => {
                write!(f, "ip link set {name} {}", if *up { "up" } else { "down" })
            }
            Op::AddBridgeVlan { dev, vid, flags } => write!(
                f,
                "bridge vlan add dev {dev} vid {vid}{}{}{}",
                if flags.pvid { " pvid" } else { "" },
                if flags.untagged { " untagged" } else { "" },
                self_flag(dev)
            ),
            Op::DelBridgeVlan { dev, vid } => {
                write!(f, "bridge vlan del dev {dev} vid {vid}{}", self_flag(dev))
            }
            Op::AddAddress { dev, prefix } => write!(f, "ip addr add {prefix} dev {dev}"),
            Op::DelAddress { dev, prefix } => write!(f, "ip addr del {prefix} dev {dev}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub trait NetBackend {
    fn observe(&mut self) -> Result<ActualState>;
    fn apply(&mut self, op: &Op) -> Result<()>;
}

/// Planning rounds before [`reconcile`] gives up. Round one does the work,
/// round two picks up side effects, round three must find nothing.
const MAX_ROUNDS: usize = 3;

/// Changes the kernel until it matches `desired`, and returns the operations
/// applied. `log` sees each operation before it is applied.
pub fn reconcile(
    net: &mut dyn NetBackend,
    desired: &DesiredState,
    log: &mut dyn FnMut(&Op),
) -> Result<Vec<Op>> {
    let mut applied = Vec::new();
    for _ in 0..MAX_ROUNDS {
        let actual = net.observe()?;
        let mut ops = actual.platform_ops();
        ops.extend(plan(desired, &actual)?);
        if ops.is_empty() {
            return Ok(applied);
        }
        for op in ops {
            log(&op);
            net.apply(&op).map_err(|e| Error(format!("{op}: {e}")))?;
            applied.push(op);
        }
    }
    Err(Error(format!(
        "kernel state did not converge after {MAX_ROUNDS} rounds"
    )))
}
