#!/usr/bin/env bash
set -euo pipefail

# DNS must reach the container's configured resolver even when it is private.
mapfile -t dns4 < <(awk '$1 == "nameserver" && $2 !~ /:/ { print $2 }' /etc/resolv.conf)
mapfile -t dns6 < <(awk '$1 == "nameserver" && $2 ~ /:/ { print $2 }' /etc/resolv.conf)

iptables -N C4AI_EGRESS
iptables -A C4AI_EGRESS -o lo -p tcp -m multiport --dports 6379,39001 -j RETURN
# Rootless Podman forwards the host-bound API through its slirp subnet.
iptables -A C4AI_EGRESS -d 10.0.2.0/24 -p tcp --dport 11235 -j RETURN
iptables -A C4AI_EGRESS -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
for ip in "${dns4[@]}"; do
  iptables -A C4AI_EGRESS -d "$ip" -p udp --dport 53 -j RETURN
  iptables -A C4AI_EGRESS -d "$ip" -p tcp --dport 53 -j RETURN
done
for cidr in 0.0.0.0/8 10.0.0.0/8 100.64.0.0/10 127.0.0.0/8 169.254.0.0/16 \
  172.16.0.0/12 192.0.0.0/24 192.0.2.0/24 192.168.0.0/16 198.18.0.0/15 \
  198.51.100.0/24 203.0.113.0/24 224.0.0.0/4 240.0.0.0/4; do
  iptables -A C4AI_EGRESS -d "$cidr" -j REJECT
done
iptables -A OUTPUT -j C4AI_EGRESS

ip6tables -N C4AI_EGRESS
ip6tables -A C4AI_EGRESS -o lo -p tcp -m multiport --dports 6379,39001 -j RETURN
ip6tables -A C4AI_EGRESS -m conntrack --ctstate ESTABLISHED,RELATED -j RETURN
for ip in "${dns6[@]}"; do
  ip6tables -A C4AI_EGRESS -d "$ip" -p udp --dport 53 -j RETURN
  ip6tables -A C4AI_EGRESS -d "$ip" -p tcp --dport 53 -j RETURN
done
for cidr in ::/128 ::1/128 ::ffff:0:0/96 64:ff9b:1::/48 100::/64 2001:2::/48 \
  2001:db8::/32 fc00::/7 fe80::/10 ff00::/8; do
  ip6tables -A C4AI_EGRESS -d "$cidr" -j REJECT
done
ip6tables -A OUTPUT -j C4AI_EGRESS

# The service never keeps NET_ADMIN; only this short root bootstrap owns it.
export HOME=/home/appuser USER=appuser LOGNAME=appuser
exec setpriv --reuid=appuser --regid=appuser --init-groups \
  --inh-caps=-all --ambient-caps=-all --bounding-set=-all --no-new-privs -- "$@"
