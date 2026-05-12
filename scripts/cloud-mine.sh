#!/usr/bin/env bash
#
# Equium cloud-mining bootstrap.
#
# Designed for a fresh Ubuntu/Debian instance (vast.ai, Runpod, Lambda,
# anything with an NVIDIA GPU + Vulkan driver). One command from "I
# just rented this box" to "I'm mining EQM":
#
#   curl -fsSL https://raw.githubusercontent.com/HannaPrints/equium/master/scripts/cloud-mine.sh -o cloud-mine.sh
#   chmod +x cloud-mine.sh
#   ./cloud-mine.sh
#
# Re-runnable: every step short-circuits if its outputs already exist.
# Ctrl-C during mining and rerun → goes straight back to mining.
# Even survives a reboot: RPC URL + keypair location are persisted in
# ~/.config/equium/ so the post-reboot run resumes cleanly.
#
# If the box ships NVIDIA driver 575.x (known SPIR-V crash bug), the
# script offers to apt-install the 535 LTS replacement and reboot for
# you. After reboot, ssh back in and rerun — you'll skip straight to
# the mining step.

set -euo pipefail

# ----- colors / log helpers -----------------------------------------
if [[ -t 1 ]]; then
  C_BOLD="\033[1m"; C_DIM="\033[2m"; C_RESET="\033[0m"
  C_ROSE="\033[38;5;204m"; C_MINT="\033[38;5;121m"; C_GOLD="\033[38;5;221m"
else
  C_BOLD=""; C_DIM=""; C_RESET=""; C_ROSE=""; C_MINT=""; C_GOLD=""
fi
info()  { printf "${C_ROSE}▸${C_RESET} %s\n" "$*"; }
ok()    { printf "${C_MINT}✓${C_RESET} %s\n" "$*"; }
warn()  { printf "${C_GOLD}!${C_RESET} %s\n" "$*"; }
fatal() { printf "\033[31m✗ %s${C_RESET}\n" "$*" >&2; exit 1; }
hr()    { printf "${C_DIM}%s${C_RESET}\n" "════════════════════════════════════════════════════════════════"; }

# ----- preflight ------------------------------------------------------
hr
printf "${C_BOLD}Equium cloud-mine bootstrap${C_RESET}\n"
hr

if [[ "$(uname)" != "Linux" ]]; then
  fatal "This script targets Linux. On macOS / Windows, follow the manual install at https://equium.xyz/download."
fi

if [[ $EUID -eq 0 ]]; then
  SUDO=""
else
  if command -v sudo >/dev/null; then
    SUDO="sudo"
  else
    fatal "Not root and no sudo available — install tools manually first."
  fi
fi

# Friendly tmux/screen hint so the user doesn't lose progress on a
# stale SSH connection. Cloud GPU rentals get expensive if a network
# blip kicks you off and the miner stops.
if [[ -z "${TMUX:-}" && -z "${STY:-}" && -t 1 ]]; then
  warn "Tip: run this inside ${C_BOLD}tmux${C_RESET}${C_GOLD} or ${C_BOLD}screen${C_RESET}${C_GOLD} so an SSH drop doesn't kill the miner."
  warn "   apt install -y tmux && tmux new -s eqm   # then ./cloud-mine.sh"
fi

# Docker detection: containers can't reboot the host, so the
# driver-downgrade flow doesn't apply there.
IN_DOCKER=0
if [[ -f /.dockerenv ]] || grep -q '/docker/\|/lxc/' /proc/1/cgroup 2>/dev/null; then
  IN_DOCKER=1
fi

# Persistent config dir — lets us remember the RPC URL across reboots
# and re-runs so the operator only enters it once.
CONFIG_DIR="${EQUIUM_CONFIG_DIR:-$HOME/.config/equium}"
mkdir -p "$CONFIG_DIR"
RPC_FILE="$CONFIG_DIR/rpc"

# ----- step 0: NVIDIA driver health ---------------------------------
# Driver 575.x ships a SPIR-V compiler bug. Offer to install 535 LTS
# automatically when we detect it. Outside Docker only — containers
# share the host's kernel module, so this needs an actual reboot.
maybe_fix_nvidia() {
  if ! command -v nvidia-smi >/dev/null; then
    return 0
  fi
  local drv
  drv=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1 | tr -d '[:space:]')
  [[ "$drv" =~ ^575\. ]] || return 0

  warn "NVIDIA driver $drv has a known SPIR-V crash bug in compute pipelines."

  if [[ $IN_DOCKER -eq 1 ]]; then
    warn "Inside a container — can't replace the kernel module from here."
    warn "The miner will auto-fall-back to the GL backend (slower but functional)."
    warn "For full speed: rent a non-Docker instance, or ask the provider for driver ≤545."
    return 0
  fi

  # If the LTS driver is already installed alongside, just suggest a
  # config switch via update-alternatives — no apt install needed.
  if dpkg -l 2>/dev/null | grep -q 'nvidia-driver-535'; then
    info "Detected nvidia-driver-535 already installed. Use it via:"
    info "  $SUDO dpkg --configure nvidia-driver-535 && $SUDO reboot"
    return 0
  fi

  printf "\n${C_BOLD}Auto-downgrade to nvidia-driver-535-server (LTS) + reboot?${C_RESET}\n"
  printf "${C_DIM}  After reboot, ssh back in and rerun ./cloud-mine.sh — it'll skip straight to mining.${C_RESET}\n"
  read -r -p "  Proceed? [y/N]: " reply
  if [[ ! "$reply" =~ ^[yY] ]]; then
    warn "Skipping driver fix. Miner will use the GL fallback for now."
    return 0
  fi

  info "Installing nvidia-driver-535-server… (this can take a minute)"
  $SUDO apt-get update -qq
  $SUDO apt-get install -y nvidia-driver-535-server
  ok "Driver installed. Rebooting in 5 seconds — reconnect after ~1 minute."
  sleep 5
  $SUDO reboot
  exit 0
}
maybe_fix_nvidia

# ----- step 1: system tools -----------------------------------------
info "Checking system tools…"
needs_apt_update=1
apt_install() {
  if [[ $needs_apt_update -eq 1 ]]; then
    $SUDO apt-get update -qq
    needs_apt_update=0
  fi
  $SUDO apt-get install -y -qq "$@"
}

if ! command -v git >/dev/null; then apt_install git; fi
if ! command -v curl >/dev/null; then apt_install curl; fi
if ! command -v pkg-config >/dev/null; then apt_install pkg-config build-essential libssl-dev; fi

# Vulkan loader — wgpu picks up NVIDIA/AMD/Intel Vulkan ICDs once this
# is in place. If we can't install it (no apt etc.) the miner's
# auto-probe falls back to GL.
if ! command -v vulkaninfo >/dev/null; then
  info "Installing Vulkan loader…"
  apt_install libvulkan1 vulkan-tools libvulkan-dev || warn "Vulkan install failed — GL fallback will be used."
fi
ok "Vulkan present"

if ! command -v cargo >/dev/null; then
  info "Installing Rust (rustup)…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi
ok "Rust $(rustc --version | awk '{print $2}')"

if ! command -v solana-keygen >/dev/null; then
  info "Installing Solana CLI…"
  sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
  export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"
fi
ok "Solana CLI present"

# ----- step 2: source + build ---------------------------------------
REPO_DIR="${EQUIUM_DIR:-$HOME/equium}"
if [[ -d "$REPO_DIR/.git" ]]; then
  info "Updating existing repo at $REPO_DIR…"
  git -C "$REPO_DIR" fetch --quiet origin master
  git -C "$REPO_DIR" reset --quiet --hard origin/master
else
  info "Cloning Equium → $REPO_DIR"
  git clone --quiet https://github.com/HannaPrints/equium "$REPO_DIR"
fi
cd "$REPO_DIR"

if [[ -x target/release/equium-gpu-miner ]] && [[ "$(uname -s)" = "Linux" ]]; then
  # Cheap freshness check: rebuild only if source touched after binary.
  src_mtime=$(stat -c %Y clients/gpu-miner/src/main.rs 2>/dev/null || echo 0)
  bin_mtime=$(stat -c %Y target/release/equium-gpu-miner 2>/dev/null || echo 0)
  if [[ "$src_mtime" -gt "$bin_mtime" ]]; then
    info "Source changed — rebuilding equium-gpu-miner…"
    cargo build --release --quiet -p equium-gpu-miner
  else
    ok "Binary up to date — skipping build"
  fi
else
  info "Building equium-gpu-miner (release)…"
  cargo build --release --quiet -p equium-gpu-miner
fi
ok "Built target/release/equium-gpu-miner"

# ----- step 3: keypair ----------------------------------------------
KEYPAIR_PATH="${EQUIUM_KEYPAIR:-$HOME/.config/solana/id.json}"
if [[ ! -f "$KEYPAIR_PATH" ]]; then
  info "Generating mining keypair at $KEYPAIR_PATH"
  mkdir -p "$(dirname "$KEYPAIR_PATH")"
  solana-keygen new --no-bip39-passphrase --force -o "$KEYPAIR_PATH" >/dev/null
  ok "Keypair created"
else
  ok "Reusing keypair at $KEYPAIR_PATH"
fi
PUBKEY=$(solana-keygen pubkey "$KEYPAIR_PATH")

# ----- step 4: GPU verify (auto-probe lives in the binary) ----------
info "Verifying GPU shader (auto-probe Vulkan → GL)…"
hr
./target/release/equium-gpu-miner verify
hr

# ----- step 5: RPC URL ----------------------------------------------
# Priority: env var > saved file > interactive prompt. Save once
# entered so a reboot/rerun skips this step.
RPC_URL="${EQUIUM_RPC_URL:-}"
if [[ -z "$RPC_URL" && -f "$RPC_FILE" ]]; then
  RPC_URL=$(<"$RPC_FILE")
  ok "Reusing saved RPC: $(printf '%.40s…' "$RPC_URL")"
fi
if [[ -z "$RPC_URL" ]]; then
  printf "\n${C_BOLD}Paste your Helius (or other) mainnet RPC URL${C_RESET}\n"
  printf "${C_DIM}  Free key: https://www.helius.dev (5-minute signup)${C_RESET}\n"
  read -r -p "RPC URL: " RPC_URL
  if [[ ! "$RPC_URL" =~ ^https?:// ]]; then
    fatal "RPC URL must start with http:// or https://"
  fi
  printf '%s\n' "$RPC_URL" > "$RPC_FILE"
  chmod 600 "$RPC_FILE"
  ok "Saved to $RPC_FILE (use \`rm $RPC_FILE\` to forget)"
fi

# ----- step 6: funding ----------------------------------------------
# Check current balance first — if already funded from a previous
# session, skip the polling loop entirely.
current_balance() {
  local raw
  raw=$(solana balance "$PUBKEY" --url "$RPC_URL" 2>/dev/null || echo "")
  if [[ "$raw" == *"SOL"* ]]; then
    echo "$raw" | awk '{print $1}'
  else
    echo "0"
  fi
}

BAL=$(current_balance)
if awk -v b="$BAL" 'BEGIN { exit !(b > 0.001) }'; then
  ok "Wallet already funded with ${BAL} SOL — straight to mining."
else
  hr
  printf "${C_BOLD}Fund this address with ~0.01 SOL for transaction fees:${C_RESET}\n\n"
  printf "  ${C_GOLD}%s${C_RESET}\n\n" "$PUBKEY"
  hr
  info "Waiting for SOL (checking every 10s; Ctrl-C to cancel)…"
  while true; do
    BAL=$(current_balance)
    if [[ "$BAL" != "0" ]]; then
      printf "  balance: ${C_MINT}%s SOL${C_RESET}\n" "$BAL"
      if awk -v b="$BAL" 'BEGIN { exit !(b > 0.001) }'; then
        break
      fi
    else
      printf "  balance: ${C_DIM}—${C_RESET}\n"
    fi
    sleep 10
  done
  ok "Funded. Starting miner…"
fi

# ----- step 7: mine -------------------------------------------------
hr
printf "${C_BOLD}Mining EQM on $PUBKEY${C_RESET}\n"
printf "${C_DIM}Press Ctrl-C to stop. Rerun ./cloud-mine.sh to resume — RPC + keypair are saved.${C_RESET}\n"
hr
exec ./target/release/equium-gpu-miner mine \
  --rpc-url "$RPC_URL" \
  --keypair "$KEYPAIR_PATH"
