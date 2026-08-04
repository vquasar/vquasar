#!/usr/bin/env bash
# Install Open vSwitch and create the integration bridge (design document,
# section 18). Run once per host before starting ch-agent with the OVS backend.
#
# Usage: scripts/setup-ovs.sh [--bridge br-int]
set -euo pipefail

BRIDGE="br-int"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bridge) BRIDGE="$2"; shift 2 ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if ! command -v ovs-vsctl >/dev/null; then
  echo "==> Installing Open vSwitch"
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y openvswitch-switch
fi

sudo systemctl enable --now openvswitch-switch
echo "==> Creating integration bridge $BRIDGE"
sudo ovs-vsctl --may-exist add-br "$BRIDGE"
sudo ovs-vsctl list-br

# VXLAN overlay networks (design M13b) tunnel over UDP 4789 between hosts. Open
# it on the underlay firewall, or overlay traffic is silently dropped on ingress
# (firewalld's default reject) even though the tunnels come up cleanly.
if command -v firewall-cmd >/dev/null && sudo firewall-cmd --state >/dev/null 2>&1; then
  echo "==> Opening VXLAN underlay port 4789/udp (firewalld)"
  sudo firewall-cmd --add-port=4789/udp --permanent >/dev/null || true
  sudo firewall-cmd --add-port=4789/udp >/dev/null || true
elif command -v ufw >/dev/null && sudo ufw status 2>/dev/null | grep -q active; then
  echo "==> Opening VXLAN underlay port 4789/udp (ufw)"
  sudo ufw allow 4789/udp || true
else
  echo "==> NOTE: ensure UDP 4789 (VXLAN) is open between hosts for overlay networks"
fi

echo
echo "Done. Start the agent with:  CH_AGENT_NETWORK__BRIDGE=$BRIDGE"
echo "The agent must run privileged (root or CAP_NET_ADMIN) to create TAPs and"
echo "attach them to $BRIDGE."
