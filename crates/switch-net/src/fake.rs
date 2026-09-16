//! In-memory [`NetBackend`] that models the kernel's behaviour closely
//! enough to test planning without netlink or privileges.

use crate::{ActualState, Error, Link, LinkKind, NetBackend, Op, Result, VlanFlags};

#[derive(Debug, Clone, Default)]
pub struct FakeNet {
    pub state: ActualState,
}

impl FakeNet {
    /// A GS1900-8 after boot: lo and the conduit eth0 down, lan1..lan8 down.
    pub fn gs1900_8() -> Self {
        let mut state = ActualState::default();
        for name in ["lo", "eth0"] {
            state.links.insert(name.into(), link(LinkKind::Other));
        }
        for i in 1..=8 {
            state.links.insert(
                format!("lan{i}"),
                link(LinkKind::Dsa {
                    conduit: Some("eth0".into()),
                }),
            );
        }
        FakeNet { state }
    }
}

fn link(kind: LinkKind) -> Link {
    Link {
        kind,
        up: false,
        master: None,
    }
}

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(Error(message.into()))
}

impl NetBackend for FakeNet {
    fn observe(&mut self) -> Result<ActualState> {
        Ok(self.state.clone())
    }

    fn apply(&mut self, op: &Op) -> Result<()> {
        let s = &mut self.state;
        match op {
            Op::CreateBridge { name } => {
                if s.links.contains_key(name) {
                    return err("File exists");
                }
                s.links.insert(
                    name.clone(),
                    link(LinkKind::Bridge {
                        vlan_filtering: true,
                        default_pvid: 0,
                    }),
                );
            }
            Op::ConfigureBridge { name } => match s.links.get_mut(name).map(|l| &mut l.kind) {
                Some(LinkKind::Bridge {
                    vlan_filtering,
                    default_pvid,
                }) => {
                    // Like the kernel, dropping the default PVID removes the
                    // default VLAN from ports that still have it unchanged.
                    let old = *default_pvid;
                    *vlan_filtering = true;
                    *default_pvid = 0;
                    if old != 0 {
                        for (port, l) in &s.links {
                            if l.master.as_deref() == Some(name) {
                                if let Some(vlans) = s.bridge_vlans.get_mut(port) {
                                    if vlans.get(&old).is_some_and(|f| f.pvid && f.untagged) {
                                        vlans.remove(&old);
                                    }
                                }
                            }
                        }
                    }
                }
                _ => return err("not a bridge"),
            },
            Op::DeleteLink { name } => {
                if s.links.remove(name).is_none() {
                    return err("Cannot find device");
                }
                s.bridge_vlans.remove(name);
                s.addresses.remove(name);
                let children: Vec<String> = s
                    .links
                    .iter()
                    .filter(
                        |(_, l)| matches!(&l.kind, LinkKind::Vlan { parent, .. } if parent == name),
                    )
                    .map(|(n, _)| n.clone())
                    .collect();
                for child in children {
                    self.apply(&Op::DeleteLink { name: child })?;
                }
                let s = &mut self.state;
                for (port, l) in s.links.iter_mut() {
                    if l.master.as_deref() == Some(name) {
                        l.master = None;
                        s.bridge_vlans.remove(port);
                    }
                }
            }
            Op::CreateVlan { name, parent, id } => {
                if s.links.contains_key(name) {
                    return err("File exists");
                }
                if !s.links.contains_key(parent) {
                    return err("Cannot find parent device");
                }
                s.links.insert(
                    name.clone(),
                    link(LinkKind::Vlan {
                        parent: parent.clone(),
                        id: *id,
                    }),
                );
            }
            Op::SetMaster { name, master } => {
                let default_pvid = match master {
                    None => 0,
                    Some(m) => match s.links.get(m).map(|l| &l.kind) {
                        Some(LinkKind::Bridge { default_pvid, .. }) => *default_pvid,
                        _ => return err("master is not a bridge"),
                    },
                };
                let Some(l) = s.links.get_mut(name) else {
                    return err("Cannot find device");
                };
                l.master = master.clone();
                s.bridge_vlans.remove(name);
                if default_pvid != 0 {
                    s.bridge_vlans.entry(name.clone()).or_default().insert(
                        default_pvid,
                        VlanFlags {
                            pvid: true,
                            untagged: true,
                        },
                    );
                }
            }
            Op::SetUp { name, up } => match s.links.get_mut(name) {
                Some(l) => l.up = *up,
                None => return err("Cannot find device"),
            },
            Op::AddBridgeVlan { dev, vid, flags } => {
                let Some(l) = s.links.get(dev) else {
                    return err("Cannot find device");
                };
                let is_bridge = matches!(l.kind, LinkKind::Bridge { .. });
                if !is_bridge && l.master.is_none() {
                    return err("Operation not supported: not a bridge port");
                }
                let vlans = s.bridge_vlans.entry(dev.clone()).or_default();
                if flags.pvid {
                    for f in vlans.values_mut() {
                        f.pvid = false;
                    }
                }
                vlans.insert(*vid, *flags);
            }
            Op::DelBridgeVlan { dev, vid } => {
                if s.bridge_vlans
                    .get_mut(dev)
                    .and_then(|v| v.remove(vid))
                    .is_none()
                {
                    return err("No such VLAN entry");
                }
            }
            Op::AddAddress { dev, prefix } => {
                if !s.links.contains_key(dev) {
                    return err("Cannot find device");
                }
                if !s.addresses.entry(dev.clone()).or_default().insert(*prefix) {
                    return err("File exists");
                }
            }
            Op::DelAddress { dev, prefix } => {
                if !s.addresses.get_mut(dev).is_some_and(|a| a.remove(prefix)) {
                    return err("Cannot assign requested address");
                }
            }
        }
        Ok(())
    }
}
