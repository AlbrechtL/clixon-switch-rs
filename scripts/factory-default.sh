#!/bin/sh
# Prints the factory default configuration as a clixon datastore: the given
# ports as access ports in VLAN 1, and interface vlan1 with the address.
#
#   scripts/factory-default.sh "lan1 lan2 ... lan8" 192.168.1.1/24

set -eu

ports=${1:?usage: $0 <ports> <address/prefix-length>}
address=${2:?usage: $0 <ports> <address/prefix-length>}
ip=${address%/*}
prefix_length=${address#*/}

cat <<XML
<config>
  <interfaces xmlns="http://openconfig.net/yang/interfaces">
XML
for port in $ports; do
    cat <<XML
    <interface>
      <name>$port</name>
      <config>
        <name>$port</name>
        <type xmlns:ianaift="urn:ietf:params:xml:ns:yang:iana-if-type">ianaift:ethernetCsmacd</type>
        <enabled>true</enabled>
      </config>
      <ethernet xmlns="http://openconfig.net/yang/interfaces/ethernet">
        <switched-vlan xmlns="http://openconfig.net/yang/vlan">
          <config>
            <interface-mode>ACCESS</interface-mode>
            <access-vlan>1</access-vlan>
          </config>
        </switched-vlan>
      </ethernet>
    </interface>
XML
done
cat <<XML
    <interface>
      <name>vlan1</name>
      <config>
        <name>vlan1</name>
        <type xmlns:ianaift="urn:ietf:params:xml:ns:yang:iana-if-type">ianaift:l3ipvlan</type>
        <enabled>true</enabled>
      </config>
      <routed-vlan xmlns="http://openconfig.net/yang/vlan">
        <config>
          <vlan>1</vlan>
        </config>
        <ipv4 xmlns="http://openconfig.net/yang/interfaces/ip">
          <addresses>
            <address>
              <ip>$ip</ip>
              <config>
                <ip>$ip</ip>
                <prefix-length>$prefix_length</prefix-length>
              </config>
            </address>
          </addresses>
        </ipv4>
      </routed-vlan>
    </interface>
  </interfaces>
</config>
XML
