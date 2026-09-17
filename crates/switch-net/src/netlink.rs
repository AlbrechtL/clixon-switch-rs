//! [`NetBackend`] on rtnetlink.
//!
//! Synchronous: clixon_backend calls the plugin from its single-threaded
//! event loop, and every request is a short round trip to the kernel.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr};

use netlink_packet_core::{
    NetlinkHeader, NetlinkMessage, NetlinkPayload, NLM_F_ACK, NLM_F_CREATE, NLM_F_DUMP, NLM_F_EXCL,
    NLM_F_REQUEST,
};
use netlink_packet_route::address::{
    AddressAttribute, AddressFlags, AddressHeaderFlags, AddressMessage,
};
use netlink_packet_route::link::{
    AfSpecBridge, BridgeBooleanOptionFlags, BridgeBooleanOptions, BridgeFlag, BridgeStpState,
    BridgeVlanInfo, BridgeVlanInfoFlags, InfoBridge, InfoData, InfoDsa, InfoKind, InfoVlan,
    LinkAttribute, LinkExtentMask, LinkFlags, LinkInfo, LinkMessage, State,
};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use netlink_sys::{protocols::NETLINK_ROUTE, Socket, SocketAddr};
use switch_model::{InterfaceState, Ipv4Prefix, BRIDGE_NAME};

use crate::{ActualState, BridgeStp, Error, Link, LinkKind, NetBackend, Op, Result, VlanFlags};

pub struct NetlinkBackend {
    socket: Socket,
    seq: u32,
    ports: Option<BTreeSet<String>>,
    /// See [`NetlinkBackend::stp_in_netns`].
    stp_in_netns: bool,
    /// Bridges with spanning tree switched on, while `stp_in_netns`.
    stp_bridges: BTreeSet<String>,
}

fn io_error(context: &'static str) -> impl Fn(io::Error) -> Error {
    move |e| Error(format!("{context}: {e}"))
}

impl NetlinkBackend {
    /// `ports` overrides which links are switch ports. By default they are
    /// the DSA user ports; a test setup without DSA names e.g. dummy links.
    pub fn new(ports: Option<BTreeSet<String>>) -> Result<Self> {
        let mut socket = Socket::new(NETLINK_ROUTE).map_err(io_error("netlink socket"))?;
        socket.bind_auto().map_err(io_error("netlink bind"))?;
        socket
            .connect(&SocketAddr::new(0, 0))
            .map_err(io_error("netlink connect"))?;
        Ok(NetlinkBackend {
            socket,
            seq: 0,
            ports,
            stp_in_netns: false,
            stp_bridges: BTreeSet::new(),
        })
    }

    /// For test setups in a network namespace other than the host's, where
    /// the kernel never leaves spanning tree to userspace: switching it on
    /// leaves stp_state 0, and the bridge is reported as if mstpd had it.
    /// mstpd still configures the bridge and sets port states, but the
    /// bridge forwards BPDUs instead of passing them up, so spanning tree
    /// does not converge.
    pub fn stp_in_netns(&mut self) {
        self.stp_in_netns = true;
    }

    /// Sends one request and collects the replies up to the final DONE (for
    /// dumps) or ACK/error (for requests with NLM_F_ACK).
    fn request(
        &mut self,
        message: RouteNetlinkMessage,
        flags: u16,
    ) -> Result<Vec<RouteNetlinkMessage>> {
        self.seq = self.seq.wrapping_add(1);
        let mut header = NetlinkHeader::default();
        header.flags = NLM_F_REQUEST | flags;
        header.sequence_number = self.seq;
        let mut packet = NetlinkMessage::new(header, NetlinkPayload::InnerMessage(message));
        packet.finalize();
        let mut buf = vec![0; packet.buffer_len()];
        packet.serialize(&mut buf);
        self.socket
            .send(&buf, 0)
            .map_err(io_error("netlink send"))?;

        let mut replies = Vec::new();
        loop {
            let (data, _) = self
                .socket
                .recv_from_full()
                .map_err(io_error("netlink receive"))?;
            let mut offset = 0;
            while offset < data.len() {
                let packet = NetlinkMessage::<RouteNetlinkMessage>::deserialize(&data[offset..])
                    .map_err(|e| Error(format!("netlink decode: {e}")))?;
                let len = packet.header.length as usize;
                if len == 0 {
                    return Err(Error("netlink: zero-length message".into()));
                }
                // NLMSG_ALIGN
                offset += (len + 3) & !3;
                if packet.header.sequence_number != self.seq {
                    continue;
                }
                match packet.payload {
                    NetlinkPayload::InnerMessage(m) => replies.push(m),
                    NetlinkPayload::Done(_) => return Ok(replies),
                    NetlinkPayload::Error(e) => {
                        return match e.code {
                            None => Ok(replies),
                            Some(code) => {
                                Err(Error(io::Error::from_raw_os_error(-code.get()).to_string()))
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn modify(&mut self, message: RouteNetlinkMessage, flags: u16) -> Result<()> {
        self.request(message, NLM_F_ACK | flags).map(|_| ())
    }

    fn dump_links(
        &mut self,
        family: AddressFamily,
        attributes: Vec<LinkAttribute>,
    ) -> Result<Vec<LinkMessage>> {
        let mut message = LinkMessage::default();
        message.header.interface_family = family;
        message.attributes = attributes;
        Ok(self
            .request(RouteNetlinkMessage::GetLink(message), NLM_F_DUMP)?
            .into_iter()
            .filter_map(|m| match m {
                RouteNetlinkMessage::NewLink(l) => Some(l),
                _ => None,
            })
            .collect())
    }

    fn index(&mut self, name: &str) -> Result<u32> {
        let mut message = LinkMessage::default();
        message.attributes.push(LinkAttribute::IfName(name.into()));
        self.request(RouteNetlinkMessage::GetLink(message), NLM_F_ACK)
            .map_err(|e| Error(format!("{name}: {e}")))?
            .into_iter()
            .find_map(|m| match m {
                RouteNetlinkMessage::NewLink(l) => Some(l.header.index),
                _ => None,
            })
            .ok_or_else(|| Error(format!("{name}: no such interface")))
    }

    fn bridge_vlan(&mut self, dev: &str, vid: u16, add: Option<VlanFlags>) -> Result<()> {
        let mut flags = BridgeVlanInfoFlags::empty();
        if let Some(f) = add {
            flags.set(BridgeVlanInfoFlags::Pvid, f.pvid);
            flags.set(BridgeVlanInfoFlags::Untagged, f.untagged);
        }
        let mut spec = Vec::new();
        // On the bridge device the entry is the bridge's own ("self"). On a
        // port the kernel's default, "master", is the bridge.
        if dev == BRIDGE_NAME {
            spec.push(AfSpecBridge::Flags(BridgeFlag::LowerDev));
        }
        spec.push(AfSpecBridge::VlanInfo(BridgeVlanInfo { flags, vid }));

        let mut message = LinkMessage::default();
        message.header.interface_family = AddressFamily::Bridge;
        message.header.index = self.index(dev)?;
        message.attributes = vec![LinkAttribute::AfSpecBridge(spec)];
        let request = match add {
            Some(_) => RouteNetlinkMessage::SetLink(message),
            None => RouteNetlinkMessage::DelLink(message),
        };
        self.modify(request, 0)
    }

    fn address(&mut self, dev: &str, prefix: &Ipv4Prefix, add: bool) -> Result<()> {
        let mut message = AddressMessage::default();
        message.header.family = AddressFamily::Inet;
        message.header.index = self.index(dev)?;
        message.header.prefix_len = prefix.prefix_len;
        let ip = IpAddr::V4(prefix.addr);
        message.attributes = vec![AddressAttribute::Local(ip), AddressAttribute::Address(ip)];
        if !add {
            return self.modify(RouteNetlinkMessage::DelAddress(message), 0);
        }
        if prefix.prefix_len < 31 {
            let host_bits = u32::MAX >> prefix.prefix_len;
            let broadcast = Ipv4Addr::from(u32::from(prefix.addr) | host_bits);
            message
                .attributes
                .push(AddressAttribute::Broadcast(broadcast));
        }
        self.modify(
            RouteNetlinkMessage::NewAddress(message),
            NLM_F_CREATE | NLM_F_EXCL,
        )
    }

    fn set_stp_state(&mut self, bridge: &str, state: BridgeStpState) -> Result<()> {
        let mut message = LinkMessage::default();
        message.header.index = self.index(bridge)?;
        message.attributes = vec![LinkAttribute::LinkInfo(vec![
            LinkInfo::Kind(InfoKind::Bridge),
            LinkInfo::Data(InfoData::Bridge(vec![InfoBridge::StpState(state)])),
        ])];
        self.modify(RouteNetlinkMessage::NewLink(message), 0)
    }

    /// Operational state of every link, by name.
    pub fn interface_states(&mut self) -> Result<BTreeMap<String, InterfaceState>> {
        let mut states = BTreeMap::new();
        for message in self.dump_links(AddressFamily::Unspec, vec![])? {
            let Some(name) = link_name(&message) else {
                continue;
            };
            let mut state = InterfaceState {
                admin_up: message.header.flags.contains(LinkFlags::Up),
                ..InterfaceState::default()
            };
            for attribute in &message.attributes {
                match attribute {
                    LinkAttribute::OperState(s) => state.oper_status = oper_status(s),
                    LinkAttribute::Address(mac) if mac.len() == 6 => {
                        state.mac = Some(
                            mac.iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<Vec<_>>()
                                .join(":"),
                        )
                    }
                    LinkAttribute::Stats64(s) => {
                        state.in_octets = s.rx_bytes;
                        state.in_pkts = s.rx_packets;
                        state.out_octets = s.tx_bytes;
                        state.out_pkts = s.tx_packets;
                    }
                    _ => {}
                }
            }
            states.insert(name.to_string(), state);
        }
        Ok(states)
    }
}

impl NetBackend for NetlinkBackend {
    fn observe(&mut self) -> Result<ActualState> {
        let links = self.dump_links(AddressFamily::Unspec, vec![])?;
        let names: BTreeMap<u32, String> = links
            .iter()
            .filter_map(|m| Some((m.header.index, link_name(m)?.to_string())))
            .collect();

        let mut state = ActualState::default();
        for message in &links {
            let Some(name) = names.get(&message.header.index) else {
                continue;
            };
            let mut kind = link_kind(message, &names);
            if let LinkKind::Bridge { stp, .. } = &mut kind {
                if self.stp_in_netns && self.stp_bridges.contains(name) {
                    *stp = BridgeStp::User;
                }
            }
            if let Some(ports) = &self.ports {
                kind = match kind {
                    LinkKind::Dsa { .. } if !ports.contains(name) => LinkKind::Other,
                    LinkKind::Other if ports.contains(name) => LinkKind::Dsa { conduit: None },
                    kind => kind,
                };
            }
            let master = message.attributes.iter().find_map(|a| match a {
                LinkAttribute::Controller(index) => names.get(index).cloned(),
                _ => None,
            });
            state.links.insert(
                name.clone(),
                Link {
                    kind,
                    up: message.header.flags.contains(LinkFlags::Up),
                    master,
                },
            );
        }

        // One VlanInfo per VID with RTEXT_FILTER_BRVLAN (not _COMPRESSED). For
        // the bridge device itself the kernel reports only its own ("self")
        // entries, and does not export BRENTRY in the flags.
        let bridge_links = self.dump_links(
            AddressFamily::Bridge,
            vec![LinkAttribute::ExtMask(vec![LinkExtentMask::Brvlan])],
        )?;
        for message in bridge_links {
            let Some(name) = names.get(&message.header.index) else {
                continue;
            };
            for attribute in &message.attributes {
                let LinkAttribute::AfSpecBridge(spec) = attribute else {
                    continue;
                };
                for entry in spec {
                    let AfSpecBridge::VlanInfo(info) = entry else {
                        continue;
                    };
                    state.bridge_vlans.entry(name.clone()).or_default().insert(
                        info.vid,
                        VlanFlags {
                            pvid: info.flags.contains(BridgeVlanInfoFlags::Pvid),
                            untagged: info.flags.contains(BridgeVlanInfoFlags::Untagged),
                        },
                    );
                }
            }
        }

        let mut message = AddressMessage::default();
        message.header.family = AddressFamily::Inet;
        for reply in self.request(RouteNetlinkMessage::GetAddress(message), NLM_F_DUMP)? {
            let RouteNetlinkMessage::NewAddress(address) = reply else {
                continue;
            };
            let Some(name) = names.get(&address.header.index) else {
                continue;
            };
            let v4 = |want_local: bool| {
                address.attributes.iter().find_map(|a| match a {
                    AddressAttribute::Local(IpAddr::V4(ip)) if want_local => Some(*ip),
                    AddressAttribute::Address(IpAddr::V4(ip)) if !want_local => Some(*ip),
                    _ => None,
                })
            };
            let Some(addr) = v4(true).or_else(|| v4(false)) else {
                continue;
            };
            let prefix = Ipv4Prefix {
                addr,
                prefix_len: address.header.prefix_len,
            };
            // IFA_FLAGS carries all 32 bits; the header only the lower 8.
            let permanent = address
                .attributes
                .iter()
                .find_map(|a| match a {
                    AddressAttribute::Flags(f) => Some(f.contains(AddressFlags::Permanent)),
                    _ => None,
                })
                .unwrap_or_else(|| address.header.flags.contains(AddressHeaderFlags::Permanent));
            if permanent {
                state
                    .addresses
                    .entry(name.clone())
                    .or_default()
                    .insert(prefix);
            } else {
                let valid = address
                    .attributes
                    .iter()
                    .find_map(|a| match a {
                        AddressAttribute::CacheInfo(c) => Some(c.ifa_valid),
                        _ => None,
                    })
                    .unwrap_or(0);
                state
                    .dhcp_addresses
                    .entry(name.clone())
                    .or_default()
                    .insert(prefix, valid);
            }
        }

        Ok(state)
    }

    fn apply(&mut self, op: &Op) -> Result<()> {
        match op {
            Op::CreateBridge { name } => {
                let mut message = LinkMessage::default();
                message.attributes = vec![LinkAttribute::IfName(name.clone()), bridge_info()];
                self.modify(
                    RouteNetlinkMessage::NewLink(message),
                    NLM_F_CREATE | NLM_F_EXCL,
                )
            }
            Op::ConfigureBridge { name } => {
                let mut message = LinkMessage::default();
                message.header.index = self.index(name)?;
                message.attributes = vec![bridge_info()];
                self.modify(RouteNetlinkMessage::NewLink(message), 0)
            }
            Op::SetBridgeStp { name, on } if self.stp_in_netns => {
                match on {
                    true => self.stp_bridges.insert(name.clone()),
                    false => self.stp_bridges.remove(name),
                };
                Ok(())
            }
            Op::SetBridgeStp { name, on: false } => {
                self.set_stp_state(name, BridgeStpState::Disabled)
            }
            Op::SetBridgeStp { name, on: true } => {
                // Requests spanning tree. The kernel asks /sbin/bridge-stp,
                // and runs its own STP (stp_state 1) unless that succeeds, or
                // leaves it to userspace (stp_state 2).
                self.set_stp_state(name, BridgeStpState::KernelStp)?;
                let kernel_stp = self
                    .dump_links(AddressFamily::Unspec, vec![])?
                    .iter()
                    .filter(|m| link_name(m) == Some(name.as_str()))
                    .any(|m| {
                        matches!(
                            link_kind(m, &BTreeMap::new()),
                            LinkKind::Bridge {
                                stp: BridgeStp::Kernel,
                                ..
                            }
                        )
                    });
                if kernel_stp {
                    self.set_stp_state(name, BridgeStpState::Disabled)?;
                    return Err(Error(
                        "the kernel did not leave spanning tree to mstpd: /sbin/bridge-stp is missing or failed".into(),
                    ));
                }
                Ok(())
            }
            Op::DeleteLink { name } => {
                self.stp_bridges.remove(name);
                let mut message = LinkMessage::default();
                message.header.index = self.index(name)?;
                self.modify(RouteNetlinkMessage::DelLink(message), 0)
            }
            Op::CreateVlan { name, parent, id } => {
                let mut message = LinkMessage::default();
                message.attributes = vec![
                    LinkAttribute::IfName(name.clone()),
                    LinkAttribute::Link(self.index(parent)?),
                    LinkAttribute::LinkInfo(vec![
                        LinkInfo::Kind(InfoKind::Vlan),
                        LinkInfo::Data(InfoData::Vlan(vec![InfoVlan::Id(*id)])),
                    ]),
                ];
                self.modify(
                    RouteNetlinkMessage::NewLink(message),
                    NLM_F_CREATE | NLM_F_EXCL,
                )
            }
            Op::SetMaster { name, master } => {
                let master = match master {
                    Some(m) => self.index(m)?,
                    None => 0,
                };
                let mut message = LinkMessage::default();
                message.header.index = self.index(name)?;
                message.attributes = vec![LinkAttribute::Controller(master)];
                self.modify(RouteNetlinkMessage::SetLink(message), 0)
            }
            Op::SetUp { name, up } => {
                let mut message = LinkMessage::default();
                message.header.index = self.index(name)?;
                message.header.change_mask = LinkFlags::Up;
                if *up {
                    message.header.flags = LinkFlags::Up;
                }
                self.modify(RouteNetlinkMessage::SetLink(message), 0)
            }
            Op::AddBridgeVlan { dev, vid, flags } => self.bridge_vlan(dev, *vid, Some(*flags)),
            Op::DelBridgeVlan { dev, vid } => self.bridge_vlan(dev, *vid, None),
            Op::AddAddress { dev, prefix } => self.address(dev, prefix, true),
            Op::DelAddress { dev, prefix } => self.address(dev, prefix, false),
        }
    }
}

fn bridge_info() -> LinkAttribute {
    LinkAttribute::LinkInfo(vec![
        LinkInfo::Kind(InfoKind::Bridge),
        LinkInfo::Data(InfoData::Bridge(vec![
            InfoBridge::VlanFiltering(true),
            InfoBridge::VlanDefaultPvid(0),
            InfoBridge::MultiBoolOpt(BridgeBooleanOptions {
                value: BridgeBooleanOptionFlags::MstEnable,
                mask: BridgeBooleanOptionFlags::MstEnable,
            }),
        ])),
    ])
}

fn link_name(message: &LinkMessage) -> Option<&str> {
    message.attributes.iter().find_map(|a| match a {
        LinkAttribute::IfName(name) => Some(name.as_str()),
        _ => None,
    })
}

fn link_kind(message: &LinkMessage, names: &BTreeMap<u32, String>) -> LinkKind {
    let mut kind = None;
    let mut data = None;
    let mut link = None;
    for attribute in &message.attributes {
        match attribute {
            LinkAttribute::LinkInfo(infos) => {
                for info in infos {
                    match info {
                        LinkInfo::Kind(k) => kind = Some(k),
                        LinkInfo::Data(d) => data = Some(d),
                        _ => {}
                    }
                }
            }
            LinkAttribute::Link(index) => link = Some(*index),
            _ => {}
        }
    }
    let name_of = |index: u32| names.get(&index).cloned();

    match (kind, data) {
        (Some(InfoKind::Dsa), data) => {
            let conduit = match data {
                Some(InfoData::Dsa(dsa)) => dsa.iter().find_map(|d| match d {
                    InfoDsa::Conduit(index) => Some(*index),
                    _ => None,
                }),
                _ => None,
            };
            LinkKind::Dsa {
                conduit: conduit.or(link).and_then(name_of),
            }
        }
        (Some(InfoKind::Bridge), data) => {
            // Kernel defaults, in case the attributes are missing.
            let mut vlan_filtering = false;
            let mut default_pvid = 1;
            let mut mst_enabled = false;
            let mut stp = BridgeStp::Off;
            if let Some(InfoData::Bridge(bridge)) = data {
                for b in bridge {
                    match b {
                        InfoBridge::VlanFiltering(v) => vlan_filtering = *v,
                        InfoBridge::VlanDefaultPvid(p) => default_pvid = *p,
                        InfoBridge::MultiBoolOpt(o) => {
                            mst_enabled = o.value.contains(BridgeBooleanOptionFlags::MstEnable)
                        }
                        InfoBridge::StpState(BridgeStpState::Disabled) => stp = BridgeStp::Off,
                        InfoBridge::StpState(BridgeStpState::UserStp) => stp = BridgeStp::User,
                        InfoBridge::StpState(_) => stp = BridgeStp::Kernel,
                        _ => {}
                    }
                }
            }
            LinkKind::Bridge {
                vlan_filtering,
                default_pvid,
                mst_enabled,
                stp,
            }
        }
        (Some(InfoKind::Vlan), Some(InfoData::Vlan(vlan))) => {
            let id = vlan.iter().find_map(|v| match v {
                InfoVlan::Id(id) => Some(*id),
                _ => None,
            });
            match (link.and_then(name_of), id) {
                (Some(parent), Some(id)) => LinkKind::Vlan { parent, id },
                _ => LinkKind::Other,
            }
        }
        _ => LinkKind::Other,
    }
}

fn oper_status(state: &State) -> &'static str {
    match state {
        State::Up => "UP",
        State::Down => "DOWN",
        State::LowerLayerDown => "LOWER_LAYER_DOWN",
        State::Testing => "TESTING",
        State::Dormant => "DORMANT",
        State::NotPresent => "NOT_PRESENT",
        _ => "UNKNOWN",
    }
}
