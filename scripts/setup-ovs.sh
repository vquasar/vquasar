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

echo
echo "Done. Start the agent with:  CH_AGENT_NETWORK__BRIDGE=$BRIDGE"
echo "The agent must run privileged (root or CAP_NET_ADMIN) to create TAPs and"
echo "attach them to $BRIDGE."
