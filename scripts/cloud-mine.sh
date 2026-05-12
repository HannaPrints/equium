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
# The script:
#   1. Installs missing tools (rustup, Vulkan loader, Solana CLI) without
#      stomping anything already present.
#   2. Clones the repo (or updates if cwd already has it).
#   3. Builds equium-gpu-miner in release mode.
#   4. Generates a mining keypair if one isn't already at the standard
#      Solana CLI path, otherwise reuses it.
#   5. Runs `verify` so the user sees their GPU is detected + the
#      WGSL shader matches the CPU reference byte-for-byte.
#   6. Prompts for a Helius RPC URL (BYOR is mandatory; the default
#      public endpoint rate-limits in seconds) and waits for the user
#      to send ~0.01 SOL for tx fees.
#   7. Execs the miner.
#
# Re-runnable: every step is idempotent. If you Ctrl-C mid-mining and
# rerun, it'll skip the install + build steps and go straight to
# verify → fund check → mine.

set -euo pipefail

# ----- colors --------------------------------------------------------
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

# ----- preflight -----------------------------------------------------
hr
printf "${C_BOLD}Equium cloud-mine bootstrap${C_RESET}\n"
hr

if [[ "$(uname)" != "Linux" ]]; then
  fatal "This script targets Linux (vast.ai / Runpod / Lambda). On macOS or Windows, follow the manual install at equium.xyz/download."
fi

# Detect whether we need sudo for apt. Cloud-GPU instances usually run
# as root; if not, fall back to sudo with a warning.
if [[ $EUID -eq 0 ]]; then
  SUDO=""
else
  if command -v sudo >/dev/null; then
    SUDO="sudo"
  else
    fatal "Not root and no sudo available — install tools manually first."
  fi
fi

# ----- step 1: tools ------------------------------------------------
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

# Vulkan loader. wgpu picks up NVIDIA's Vulkan ICD automatically once
# the loader is in place.
if ! command -v vulkaninfo >/dev/null; then
  info "Installing Vulkan loader…"
  apt_install libvulkan1 vulkan-tools libvulkan-dev
fi
ok "Vulkan present"

# Rust toolchain.
if ! command -v cargo >/dev/null; then
  info "Installing Rust (rustup)…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi
ok "Rust $(rustc --version | awk '{print $2}')"

# Solana CLI (just for keypair gen + balance check).
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

info "Building equium-gpu-miner (release)…"
cargo build --release --quiet -p equium-gpu-miner
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

# ----- step 4: GPU verify -------------------------------------------
# The miner self-probes its wgpu backends in subprocesses: a SPIR-V
# crash in the driver kills the child cleanly and the parent
# auto-falls-back to GL. No env-var fiddling needed on the user's
# side — we just print whatever choice it lands on.

info "Verifying GPU shader…"
hr
./target/release/equium-gpu-miner verify
hr

# ----- step 5: RPC + funding ----------------------------------------
RPC_URL="${EQUIUM_RPC_URL:-}"
if [[ -z "$RPC_URL" ]]; then
  printf "\n${C_BOLD}Paste your Helius (or other) mainnet RPC URL${C_RESET}\n"
  printf "${C_DIM}  Free key: https://www.helius.dev (5-minute signup)${C_RESET}\n"
  read -r -p "RPC URL: " RPC_URL
  if [[ ! "$RPC_URL" =~ ^https?:// ]]; then
    fatal "RPC URL must start with http:// or https://"
  fi
fi

hr
printf "${C_BOLD}Fund this address with ~0.01 SOL for transaction fees:${C_RESET}\n\n"
printf "  ${C_GOLD}%s${C_RESET}\n\n" "$PUBKEY"
hr
info "Waiting for SOL to arrive (checking every 10s; Ctrl-C to cancel)…"

while true; do
  BAL_RAW=$(solana balance "$PUBKEY" --url "$RPC_URL" 2>/dev/null || echo "")
  if [[ "$BAL_RAW" == *"SOL"* ]]; then
    BAL_NUM=$(echo "$BAL_RAW" | awk '{print $1}')
    printf "  balance: ${C_MINT}%s SOL${C_RESET}\n" "$BAL_NUM"
    # Compare via awk so we handle the decimal compare without bc.
    if awk -v b="$BAL_NUM" 'BEGIN { exit !(b > 0.001) }'; then
      break
    fi
  else
    printf "  balance: ${C_DIM}—${C_RESET}\n"
  fi
  sleep 10
done

ok "Funded. Starting miner…"
hr
exec ./target/release/equium-gpu-miner mine \
  --rpc-url "$RPC_URL" \
  --keypair "$KEYPAIR_PATH"
