use std::collections::{BTreeMap, BTreeSet};

use switch_model::Port;

use crate::{
    ActualState, BridgeStp, DesiredState, Error, LinkKind, Op, Result, VlanFlags, BRIDGE_NAME,
};

const ACCESS: VlanFlags = VlanFlags {
    pvid: true,
    untagged: true,
};
const TAGGED: VlanFlags = VlanFlags {
    pvid: false,
    untagged: false,
};

/// Operations that turn `actual` into `desired`, in an order the kernel
/// accepts.
///
/// Owned by the plugin, and therefore changed or removed when they do not
/// match: the bridge, the switch ports (DSA user ports), and VLAN links on the
/// bridge. Any other link is left alone, and an SVI whose name is taken by one
/// of them is an error.
///
/// Spanning tree is switched on before ports join the bridge, so that they
/// never flood BPDUs.
///
/// New addresses are added before stale ones are removed, so that moving the
/// management address does not pass through a state without one. Addresses
/// with a lifetime belong to the DHCP client: they stay on the SVI that runs
/// it and are removed everywhere else.
pub fn plan(desired: &DesiredState, actual: &ActualState) -> Result<Vec<Op>> {
    let br = BRIDGE_NAME;
    let empty_vlans = BTreeMap::new();
    let empty_addrs = BTreeSet::new();
    let empty_dhcp = BTreeMap::new();
    let mut ops = Vec::new();

    for name in desired.svis.keys() {
        if let Some(link) = actual.links.get(name) {
            let ours = matches!(&link.kind, LinkKind::Vlan { parent, .. } if parent == br);
            if !ours {
                return Err(Error(format!(
                    "interface {name} already exists and is not a VLAN link of {br}"
                )));
            }
        }
    }

    // The bridge. A fresh one has no ports, VLANs or children, whatever
    // `actual` says about the link it replaces.
    let bridge = actual.links.get(br);
    let fresh_bridge = match bridge.map(|l| &l.kind) {
        Some(LinkKind::Bridge {
            vlan_filtering: true,
            default_pvid: 0,
            mst_enabled: true,
            ..
        }) => false,
        Some(LinkKind::Bridge {
            mst_enabled,
            default_pvid,
            ..
        }) if *mst_enabled || !port_vlans_remain(actual, *default_pvid) => {
            ops.push(Op::ConfigureBridge { name: br.into() });
            false
        }
        Some(_) => {
            ops.push(Op::DeleteLink { name: br.into() });
            ops.push(Op::CreateBridge { name: br.into() });
            true
        }
        None => {
            ops.push(Op::CreateBridge { name: br.into() });
            true
        }
    };
    if fresh_bridge || bridge.is_some_and(|l| !l.up) {
        ops.push(Op::SetUp {
            name: br.into(),
            up: true,
        });
    }
    let stp = match (fresh_bridge, bridge.map(|l| &l.kind)) {
        (false, Some(LinkKind::Bridge { stp, .. })) => *stp,
        _ => BridgeStp::Off,
    };
    let want_stp = desired.stp.is_some();
    if stp == BridgeStp::Kernel {
        ops.push(Op::SetBridgeStp {
            name: br.into(),
            on: false,
        });
    }
    if want_stp != (stp == BridgeStp::User) {
        ops.push(Op::SetBridgeStp {
            name: br.into(),
            on: want_stp,
        });
    }

    // VLAN links on the bridge that are no longer wanted, or have the wrong id.
    let mut existing_svis = BTreeSet::new();
    if !fresh_bridge {
        for (name, link) in &actual.links {
            let LinkKind::Vlan { parent, id } = &link.kind else {
                continue;
            };
            if parent != br {
                continue;
            }
            if desired.svis.get(name).is_some_and(|svi| svi.vlan == *id) {
                existing_svis.insert(name.as_str());
            } else {
                ops.push(Op::DeleteLink { name: name.clone() });
            }
        }
    }

    // Switch ports.
    for (name, link) in &actual.links {
        if !matches!(link.kind, LinkKind::Dsa { .. }) {
            continue;
        }
        let enslaved = !fresh_bridge && link.master.as_deref() == Some(br);
        match desired.ports.get(name) {
            Some(port) => {
                if !enslaved {
                    ops.push(Op::SetMaster {
                        name: name.clone(),
                        master: Some(br.into()),
                    });
                }
                let have = match enslaved {
                    true => actual.bridge_vlans.get(name).unwrap_or(&empty_vlans),
                    false => &empty_vlans,
                };
                sync_vlans(&mut ops, name, have, &port_vlans(port));
                if link.up != port.enabled {
                    ops.push(Op::SetUp {
                        name: name.clone(),
                        up: port.enabled,
                    });
                }
            }
            None => {
                if link.master.is_some() {
                    ops.push(Op::SetMaster {
                        name: name.clone(),
                        master: None,
                    });
                }
                if link.up {
                    ops.push(Op::SetUp {
                        name: name.clone(),
                        up: false,
                    });
                }
            }
        }
    }

    // The bridge's own VLAN entries: one tagged entry per SVI, so the CPU
    // sees that VLAN. None for suspended VLANs.
    let have = match fresh_bridge {
        true => &empty_vlans,
        false => actual.bridge_vlans.get(br).unwrap_or(&empty_vlans),
    };
    let want = desired
        .svis
        .values()
        .filter(|svi| desired.vlan_active(svi.vlan))
        .map(|svi| (svi.vlan, TAGGED))
        .collect();
    sync_vlans(&mut ops, br, have, &want);

    // SVIs.
    for (name, svi) in &desired.svis {
        let existing = existing_svis.contains(name.as_str());
        if !existing {
            ops.push(Op::CreateVlan {
                name: name.clone(),
                parent: br.into(),
                id: svi.vlan,
            });
        }
        let (have, dhcp) = match existing {
            true => (
                actual.addresses.get(name).unwrap_or(&empty_addrs),
                actual.dhcp_addresses.get(name).unwrap_or(&empty_dhcp),
            ),
            false => (&empty_addrs, &empty_dhcp),
        };
        for prefix in svi.addresses.difference(have) {
            // A static address the DHCP client added first: the kernel only
            // makes it permanent when it is added again.
            if dhcp.contains_key(prefix) {
                ops.push(Op::DelAddress {
                    dev: name.clone(),
                    prefix: *prefix,
                });
            }
            ops.push(Op::AddAddress {
                dev: name.clone(),
                prefix: *prefix,
            });
        }
        for prefix in have.difference(&svi.addresses) {
            ops.push(Op::DelAddress {
                dev: name.clone(),
                prefix: *prefix,
            });
        }
        if !svi.dhcp_client {
            for prefix in dhcp.keys().filter(|p| !svi.addresses.contains(p)) {
                ops.push(Op::DelAddress {
                    dev: name.clone(),
                    prefix: *prefix,
                });
            }
        }
        let up = existing && actual.links[name].up;
        if up != svi.enabled {
            ops.push(Op::SetUp {
                name: name.clone(),
                up: svi.enabled,
            });
        }
    }

    // Addresses on the bridge or on ports, e.g. left over from the static
    // network script. Last, once the SVIs carry theirs.
    let leftovers = actual
        .addresses
        .iter()
        .flat_map(|(dev, addrs)| addrs.iter().map(move |p| (dev, p)))
        .chain(
            actual
                .dhcp_addresses
                .iter()
                .flat_map(|(dev, addrs)| addrs.keys().map(move |p| (dev, p))),
        );
    for (dev, prefix) in leftovers {
        let owned = match actual.links.get(dev).map(|l| &l.kind) {
            Some(LinkKind::Dsa { .. }) => true,
            Some(LinkKind::Bridge { .. }) => dev == br && !fresh_bridge,
            _ => false,
        };
        if owned {
            ops.push(Op::DelAddress {
                dev: dev.clone(),
                prefix: *prefix,
            });
        }
    }

    Ok(ops)
}

/// Whether a port of the bridge keeps VLAN entries when ConfigureBridge sets
/// vlan_default_pvid from `default_pvid` to 0, which removes the default
/// entries: then the kernel refuses to enable MST.
fn port_vlans_remain(actual: &ActualState, default_pvid: u16) -> bool {
    actual
        .links
        .iter()
        .filter(|(_, l)| l.master.as_deref() == Some(BRIDGE_NAME))
        .filter_map(|(name, _)| actual.bridge_vlans.get(name))
        .any(|vlans| {
            vlans
                .iter()
                .any(|(vid, flags)| *vid != default_pvid || *flags != ACCESS)
        })
}

/// Bridge VLAN entries of a port: its native VLAN untagged and as PVID, the
/// others tagged.
fn port_vlans(port: &Port) -> BTreeMap<u16, VlanFlags> {
    port.tagged_vlans
        .iter()
        .map(|vid| (*vid, TAGGED))
        .chain(port.native_vlan.map(|vid| (vid, ACCESS)))
        .collect()
}

fn sync_vlans(
    ops: &mut Vec<Op>,
    dev: &str,
    have: &BTreeMap<u16, VlanFlags>,
    want: &BTreeMap<u16, VlanFlags>,
) {
    // Add first, the PVID entry before all others: a port keeps a PVID while
    // its native VLAN changes, also when the old one stays as a tagged VLAN.
    let (pvid, others): (Vec<_>, Vec<_>) = want.iter().partition(|(_, flags)| flags.pvid);
    for (vid, flags) in pvid.into_iter().chain(others) {
        if have.get(vid) != Some(flags) {
            ops.push(Op::AddBridgeVlan {
                dev: dev.into(),
                vid: *vid,
                flags: *flags,
            });
        }
    }
    for vid in have.keys() {
        if !want.contains_key(vid) {
            ops.push(Op::DelBridgeVlan {
                dev: dev.into(),
                vid: *vid,
            });
        }
    }
}
