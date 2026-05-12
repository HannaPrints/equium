#!/usr/bin/env bash
#
# Equium GPU miner — auto-install + mine.
#
# Works on any Ubuntu/Debian Linux box — your personal NVIDIA/AMD
# machine, a vast.ai or Runpod rental, anything with `apt`. One
# command from "I have a GPU" to "I'm mining EQM":
#
#   curl -fsSL https://raw.githubusercontent.com/HannaPrints/equium/master/scripts/install.sh -o equium-install.sh
#   chmod +x equium-install.sh
#   ./equium-install.sh
#
# What it does:
#   1. Installs Rust, Solana CLI, Vulkan loader, and nvcc (if NVIDIA).
#   2. Clones + builds equium-gpu-miner (with --features cuda when
#      nvcc is present, so the CUDA Wagner kernels are baked in).
#   3. Generates a mining keypair, shows you the seed phrase to back up.
#   4. Prompts for an RPC URL, validates it's mainnet.
#   5. Waits for the wallet to be funded with a bit of SOL.
#   6. Execs the miner with `--full-gpu` (the fast path) when CUDA
#      is built in. Falls back to the hybrid path if you set
#      EQUIUM_HYBRID=1.
#
# Re-runnable: every step short-circuits if its outputs already exist.
# Ctrl-C during mining and rerun → goes straight back to mining. Even
# survives a reboot: RPC URL + keypair location are persisted in
# ~/.config/equium/ so the post-reboot run resumes cleanly.
#
# If the box ships NVIDIA driver 575.x (known SPIR-V crash bug), the
# script offers to apt-install the 535 LTS replacement and reboot for
# you. After reboot, rerun — you'll skip straight to the mining step.

set -euo pipefail

# ----- colors / log helpers -----------------------------------------
if [[ -t 1 ]]; then
  C_BOLD="\033[1m"; C_DIM="\033[2m"; C_RESET="\033[0m"
  C_ROSE="\033[38;5;204m"; C_MINT="\033[38;5;121m"; C_GOLD="\033[38;5;221m"
else
  C_BOLD=""; C_DIM=""; C_RESET=""; C_ROSE=""; C_MINT=""; C_GOLD=""
fi

# How the operator invoked us — used for "rerun ./equium-install.sh"
# style hints so the message matches whatever they named the file.
SELF_CMD="${0##*/}"
[[ "$SELF_CMD" == bash || -z "$SELF_CMD" ]] && SELF_CMD="install.sh"
SELF="./$SELF_CMD"
info()  { printf "${C_ROSE}▸${C_RESET} %s\n" "$*"; }
ok()    { printf "${C_MINT}✓${C_RESET} %s\n" "$*"; }
warn()  { printf "${C_GOLD}!${C_RESET} %s\n" "$*"; }
fatal() { printf "\033[31m✗ %s${C_RESET}\n" "$*" >&2; exit 1; }
hr()    { printf "${C_DIM}%s${C_RESET}\n" "════════════════════════════════════════════════════════════════"; }

# Run a long-running command behind a spinner so the operator sees
# elapsed time instead of a wall of compiler warnings. Stdout + stderr
# are captured to a temp log; on success it's discarded, on failure
# it's preserved so the operator can debug.
#
# Usage: spin "Building miner" cargo build --release --quiet -p equium-gpu-miner
spin() {
  local label="$1"; shift
  if [[ ! -t 1 ]]; then
    # Non-TTY (CI, redirect to file) — just run it loudly.
    "$@"
    return
  fi
  local log
  log=$(mktemp -t equium-spin-XXXXXX.log)
  "$@" >"$log" 2>&1 &
  local pid=$!
  local start; start=$(date +%s)
  local i=0
  # shellcheck disable=SC2034
  local frames='⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏'
  # Trap Ctrl-C so we kill the child if the user gives up.
  trap "kill $pid 2>/dev/null; printf '\r\033[K'; exit 130" INT TERM
  while kill -0 "$pid" 2>/dev/null; do
    local elapsed=$(( $(date +%s) - start ))
    local frame="${frames:i:1}"
    i=$(( (i + 1) % ${#frames} ))
    printf "\r${C_ROSE}▸${C_RESET} %s ${C_GOLD}%s${C_RESET}  ${C_DIM}%ds${C_RESET}   " \
      "$label" "$frame" "$elapsed"
    sleep 0.15
  done
  wait "$pid"
  local rc=$?
  trap - INT TERM
  local total=$(( $(date +%s) - start ))
  printf "\r\033[K"
  if [[ $rc -eq 0 ]]; then
    ok "$label done in ${total}s"
    rm -f "$log"
  else
    printf "\033[31m✗ %s failed (rc=%d, %ds)${C_RESET}\n" "$label" "$rc" "$total" >&2
    printf "  ${C_DIM}Last 30 lines of output:${C_RESET}\n"
    tail -30 "$log" | sed 's/^/    /'
    printf "\n  ${C_DIM}Full log: %s${C_RESET}\n" "$log"
    exit "$rc"
  fi
}

# ----- doctor mode --------------------------------------------------
# `./install.sh --doctor` (or `./equium-install.sh --doctor`)
# prints a single markdown report the
# operator can paste into a GitHub issue or a chat. Captures the
# things that actually matter for debugging a cloud-rented miner:
# OS / GPU / drivers / Vulkan / disk / RAM / tool versions / repo
# state / recent log tails. Redacts the Helius API key out of any RPC
# URL it finds. Doesn't read keypair files.
doctor() {
  # We're allowed (and expected) to call commands that don't exist
  # here. Disable -e + pipefail just for the doctor function so a
  # missing nvidia-smi / vulkaninfo doesn't abort the whole report.
  set +e
  set +o pipefail
  local out
  out=$(mktemp -t equium-doctor-XXXXXX.md)
  exec 3>"$out"

  # Helper: redact api-key=… from URLs.
  redact() { sed -E 's#api-key=[A-Za-z0-9_-]+#api-key=<redacted>#g; s#token=[A-Za-z0-9_-]+#token=<redacted>#g'; }

  # Helper: capture a command's output (stdout + stderr) into the
  # report with a fenced section header. Truncates long output.
  cap() {
    local title="$1"; shift
    printf '## %s\n```\n' "$title" >&3
    "$@" 2>&1 | redact | head -40 >&3
    printf '```\n\n' >&3
  }
  capfile() {
    local title="$1" path="$2"
    printf '## %s\n```\n' "$title" >&3
    if [[ -f "$path" ]]; then
      tail -30 "$path" 2>/dev/null | redact >&3
    else
      printf '(not present)\n' >&3
    fi
    printf '```\n\n' >&3
  }

  printf '# Equium doctor report\n\n_%s_\n\n' "$(date -u +%FT%TZ)" >&3
  cap "OS"           bash -c 'uname -a; cat /etc/os-release 2>/dev/null | head -6'
  cap "CPU + RAM"    bash -c 'lscpu 2>/dev/null | grep -E "Model name|Socket|Thread|Core|CPU\(s\)" | head -8; echo "---"; free -h 2>/dev/null'
  cap "Disk"         bash -c 'df -hT "$HOME" / 2>/dev/null'
  cap "Container"    bash -c '[[ -f /.dockerenv ]] && echo "docker: yes" || echo "docker: no"; head -2 /proc/1/cgroup 2>/dev/null'
  cap "PCI display"  bash -c 'lspci 2>/dev/null | grep -iE "vga|3d controller|display" | head -4'
  cap "NVIDIA"       bash -c 'command -v nvidia-smi >/dev/null && nvidia-smi --query-gpu=name,driver_version,memory.total,utilization.gpu,temperature.gpu --format=csv 2>/dev/null || echo "(no nvidia-smi)"'
  cap "Vulkan"       bash -c 'command -v vulkaninfo >/dev/null && (vulkaninfo --summary 2>&1 | head -25) || echo "(no vulkaninfo)"'
  cap "/dev nodes"   bash -c 'ls -la /dev/dri/ 2>/dev/null; ls /dev/nvidia* 2>/dev/null'
  cap "Tool versions" bash -c '
    command -v rustc >/dev/null && rustc --version || echo "rustc: missing"
    command -v cargo >/dev/null && cargo --version || echo "cargo: missing"
    command -v solana >/dev/null && solana --version || echo "solana: missing"
    command -v solana-keygen >/dev/null && solana-keygen --version || echo "solana-keygen: missing"'

  local repo="${EQUIUM_DIR:-$HOME/equium}"
  cap "Repo"         bash -c "
    if [[ -d '$repo/.git' ]]; then
      cd '$repo'
      echo path: '$repo'
      git log -1 --oneline 2>/dev/null
      echo built: \$(test -x target/release/equium-gpu-miner && stat -c %y target/release/equium-gpu-miner 2>/dev/null || echo 'no binary')
    else
      echo '(no repo at $repo)'
    fi"

  cap "Wallet" bash -c '
    KP="${EQUIUM_KEYPAIR:-$HOME/.config/solana/id.json}"
    if [[ -f "$KP" ]]; then
      PK=$(solana-keygen pubkey "$KP" 2>/dev/null || echo "?")
      echo "keypair: $KP (exists, $(stat -c %s "$KP" 2>/dev/null) bytes)"
      echo "pubkey: $PK"
      # Balance only if RPC is set — we already redact above.
      RPC_F="$HOME/.config/equium/rpc"
      if [[ -f "$RPC_F" ]]; then
        URL=$(cat "$RPC_F")
        BAL=$(solana balance "$PK" --url "$URL" 2>/dev/null || echo "(query failed)")
        echo "balance: $BAL"
      else
        echo "balance: (no saved RPC)"
      fi
    else
      echo "(no keypair at $KP)"
    fi'

  cap "Backend probe (live)" bash -c "
    BIN='$repo/target/release/equium-gpu-miner'
    if [[ -x \"\$BIN\" ]]; then
      \"\$BIN\" probe --backend vulkan 2>&1 | head -3
      \"\$BIN\" probe --backend gl 2>&1 | head -3
    else
      echo '(miner not built)'
    fi"

  capfile "Onstart log"         /var/log/equium-onstart.log
  capfile "Bootstrap log (last build)" "/tmp/$(ls -1t /tmp/equium-spin-*.log 2>/dev/null | head -1 | xargs -I{} basename {})"

  exec 3>&-
  cat "$out"
  echo
  echo "→ Report written to $out"
  echo "→ Paste the block above into an Equium issue / chat for debugging help."
}

# Arg parse: a single `--doctor` flag short-circuits everything below.
if [[ "${1:-}" == "--doctor" || "${1:-}" == "-d" ]]; then
  doctor
  exit 0
fi

# Fast-path reattach: a previous run left a detached tmux session
# mining away. Just hand the operator back to it — no need to re-run
# any of the install/build/verify dance. Stop the session explicitly
# (Ctrl-C inside or `tmux kill-session -t eqm`) to drop into a normal
# install run.
TMUX_SESSION="eqm"
if [[ -z "${TMUX:-}" ]] && command -v tmux >/dev/null \
   && tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
  printf "${C_BOLD}Miner already running${C_RESET}  ${C_DIM}— reattaching to tmux session '$TMUX_SESSION'${C_RESET}\n"
  printf "${C_DIM}  Ctrl-B then D to detach (miner keeps running).${C_RESET}\n"
  printf "${C_DIM}  tmux kill-session -t $TMUX_SESSION   # stop the miner${C_RESET}\n"
  sleep 1
  exec tmux attach -t "$TMUX_SESSION"
fi

# ----- preflight ------------------------------------------------------
hr
printf "${C_BOLD}Equium GPU miner — auto-install${C_RESET}\n"
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

# ----- pre-flight: catch show-stoppers before we waste 5 minutes ----
# Each check either auto-fixes or bails with an actionable message
# pointing at the right next step. Idempotent so a re-run after a
# reboot picks up where we left off.

# Detect Docker early — several pre-flight checks behave differently
# inside containers (no swap, no reboot, no kernel module replace).
IN_DOCKER=0
if [[ -f /.dockerenv ]] || grep -q '/docker/\|/lxc/' /proc/1/cgroup 2>/dev/null; then
  IN_DOCKER=1
fi

# Disk: cargo build's target/ + deps weigh ~3 GB. Bail early if HOME's
# partition is tighter than that so the operator can rent a bigger
# instance instead of waiting for the build to fail.
disk_avail_gb=$(df -BG --output=avail "$HOME" 2>/dev/null | tail -1 | tr -dc '0-9')
if [[ -n "$disk_avail_gb" && "$disk_avail_gb" -lt 5 ]]; then
  fatal "Only ${disk_avail_gb} GB free in \$HOME — cargo build needs ~3 GB + Solana CLI + repo. Rent an instance with ≥10 GB disk, or set EQUIUM_DIR to a larger volume."
fi

# RAM: cargo build with optimizations needs ~2 GB during link. Small
# vast.ai shapes (2-4 GB) OOM mid-build. Auto-add a 4 GB swap file
# if we're short. Skip in Docker (containers usually can't swapon).
ram_total_mb=$(awk '/MemTotal/ {print int($2 / 1024)}' /proc/meminfo 2>/dev/null || echo 0)
if [[ "$ram_total_mb" -gt 0 && "$ram_total_mb" -lt 2048 ]] && [[ $IN_DOCKER -eq 0 ]]; then
  # Already have enough swap?
  swap_mb=$(awk '/SwapTotal/ {print int($2 / 1024)}' /proc/meminfo 2>/dev/null || echo 0)
  if [[ "$swap_mb" -lt 2048 ]]; then
    warn "Only ${ram_total_mb} MB RAM detected — cargo build will likely OOM."
    printf "${C_BOLD}Add a 4 GB swap file at /equium-swap?${C_RESET} [Y/n]: "
    read -r reply
    if [[ ! "$reply" =~ ^[nN] ]]; then
      info "Creating /equium-swap (this takes ~20s)…"
      $SUDO dd if=/dev/zero of=/equium-swap bs=1M count=4096 status=none
      $SUDO chmod 600 /equium-swap
      $SUDO mkswap /equium-swap >/dev/null
      $SUDO swapon /equium-swap
      ok "Swap added — total now $(awk '/SwapTotal/ {print int($2 / 1024)}' /proc/meminfo) MB"
    fi
  fi
fi

# GPU presence: confirm there's actually a GPU we can talk to before
# we install Vulkan + Rust + Solana CLI + build the miner. Saves the
# operator from going through 5 min of setup only to find out their
# "GPU instance" didn't actually provision one.
gpu_present() {
  command -v nvidia-smi >/dev/null && return 0
  # AMD: /sys/class/drm/card0 has vendor 0x1002. Also `lspci`.
  if command -v lspci >/dev/null; then
    lspci -nn 2>/dev/null | grep -qiE 'vga|3d controller|display' && return 0
  fi
  [[ -e /dev/dri/card0 || -e /dev/nvidia0 ]] && return 0
  return 1
}
if ! gpu_present; then
  warn "No GPU detected (no nvidia-smi, no /dev/dri, no PCI display device)."
  warn "If this is supposed to be a GPU instance, the provider may have shipped a non-GPU node."
  warn "Falling back to CPU-only mining isn't going to be profitable on a cloud instance —"
  warn "the browser miner (https://equium.xyz/mine) is a better option for CPU/no-GPU users."
  printf "${C_BOLD}Continue anyway?${C_RESET} [y/N]: "
  read -r reply
  [[ "$reply" =~ ^[yY] ]] || fatal "Stopping. Rent a GPU instance or use https://equium.xyz/mine."
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
  # The NVIDIA open-driver branch (555 / 565 / 570 / 575) all ship a
  # SPIR-V compiler that fails on Naga output during compute pipeline
  # creation — surfaces as wgpu's "Parent device is lost" validation
  # error mid-init. Confirmed user-reports: 555.52.04 and 575.51.03.
  # The 535/545/550 LTS branches are healthy.
  [[ "$drv" =~ ^(555|565|570|575)\. ]] || return 0

  warn "NVIDIA driver $drv has a known SPIR-V crash bug in compute pipelines."

  if [[ $IN_DOCKER -eq 1 ]]; then
    fatal "Inside a container — driver replacement requires kernel-module
        access we don't have. This instance won't run the GPU miner.

        Realistic options:
          1. Stop this rental, pick another vast.ai / Runpod listing
             with driver 535.x / 545.x / 550.x (avoid 555-575).
          2. Use the CPU miner here instead:
               cargo build --release -p equium-cli-miner
               ./target/release/equium-miner \\
                 --rpc-url \$RPC --keypair \$KEY --threads \$(nproc)
             (won't beat GPU economics — only useful if the rental
              is already paid for and you don't want to waste it.)"
  fi

  # If the LTS driver is already installed alongside, just suggest a
  # config switch via update-alternatives — no apt install needed.
  if dpkg -l 2>/dev/null | grep -q 'nvidia-driver-535'; then
    info "Detected nvidia-driver-535 already installed. Use it via:"
    info "  $SUDO dpkg --configure nvidia-driver-535 && $SUDO reboot"
    return 0
  fi

  printf "\n${C_BOLD}Auto-downgrade to nvidia-driver-535-server (LTS) + reboot?${C_RESET}\n"
  printf "${C_DIM}  After reboot, rerun $SELF — it'll skip straight to mining.${C_RESET}\n"
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

# Make sure PATH picks up rust + solana additions in this shell AND
# any future ones (write to .bashrc once), so the operator never has
# to "restart your shell". Defined here because the Vulkan + XDG
# setup below needs it too.
ensure_path_line() {
  local line="$1"
  local rc="$HOME/.bashrc"
  [[ -f "$rc" ]] || touch "$rc"
  grep -qF "$line" "$rc" 2>/dev/null || printf '\n%s\n' "$line" >> "$rc"
}

if ! command -v git >/dev/null; then apt_install git; fi
if ! command -v curl >/dev/null; then apt_install curl; fi
if ! command -v pkg-config >/dev/null; then apt_install pkg-config build-essential libssl-dev; fi
# tmux lets the miner survive SSH disconnects — see the launch step
# at the bottom. <1 MB, install once and forget.
if ! command -v tmux >/dev/null; then apt_install tmux; fi

# Vulkan loader — wgpu picks up NVIDIA/AMD/Intel Vulkan ICDs once this
# is in place. If we can't install it (no apt etc.) the miner's
# auto-probe falls back to GL.
# NVIDIA's Vulkan ICD has a hard dependency on libXext.so.6 even for
# pure-compute workloads (the loader tries to enumerate display-related
# extensions before the app picks the headless path). Minimal CUDA
# container images don't ship the X11 client libs, so without these
# the ICD fails to load and `verify` reports "no compatible GPU
# adapter found" despite a healthy NVIDIA driver.
if ! command -v vulkaninfo >/dev/null || ! ldconfig -p | grep -q libXext.so.6; then
  spin "Installing Vulkan loader + X11 client libs" \
    $SUDO apt-get install -y -qq \
      libvulkan1 vulkan-tools libvulkan-dev \
      libxext6 libx11-6 libxcb1 libxkbcommon0 \
      || warn "Vulkan install failed — GL fallback will be used."
fi
ok "Vulkan present"

# Vulkan loader checks XDG_RUNTIME_DIR even on headless compute, and
# headless cloud containers usually don't set it. Pick a safe default
# now so the miner doesn't spam loader warnings on every probe.
if [[ -z "${XDG_RUNTIME_DIR:-}" ]]; then
  export XDG_RUNTIME_DIR="/tmp/runtime-$(id -u)"
  mkdir -p "$XDG_RUNTIME_DIR"
  chmod 700 "$XDG_RUNTIME_DIR"
  ensure_path_line "export XDG_RUNTIME_DIR=\"/tmp/runtime-\$(id -u)\""
fi

if ! command -v cargo >/dev/null; then
  spin "Installing Rust toolchain" \
    bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | \
             sh -s -- -y --no-modify-path --default-toolchain stable --profile minimal'
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
  ensure_path_line 'source "$HOME/.cargo/env"'
fi
ok "Rust $(rustc --version 2>/dev/null | awk '{print $2}')"

if ! command -v solana-keygen >/dev/null; then
  spin "Installing Solana CLI" \
    bash -c 'sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"'
  export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"
  ensure_path_line 'export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"'
fi
ok "Solana CLI present"

# CUDA toolkit — when an NVIDIA GPU is here we'd rather build the
# CUDA backend, since it bypasses the SPIR-V driver crash that
# bricks Vulkan on the 555–575 driver branch. The runtime CUDA base
# image vast.ai ships doesn't include nvcc; install it on demand.
USE_CUDA=0
if command -v nvidia-smi >/dev/null; then
  if ! command -v nvcc >/dev/null; then
    spin "Installing CUDA toolkit (nvcc)" \
      $SUDO apt-get install -y -qq nvidia-cuda-toolkit \
        || warn "nvcc install failed — building wgpu-only path instead."
  fi
  if command -v nvcc >/dev/null; then
    USE_CUDA=1
    ok "CUDA toolkit present ($(nvcc --version 2>/dev/null | grep -oE 'V[0-9.]+' | head -1))"
  fi
fi

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

# Build invocation. With CUDA enabled we add --features cuda so the
# CUDA backend (and its build.rs nvcc step) is compiled in. Cache the
# feature choice in a marker file so a re-run with toggled CUDA
# availability forces a rebuild.
cuda_marker=target/release/.equium-cuda-feature
build_cmd=(cargo build --release --quiet -p equium-gpu-miner)
expected_marker="0"
if [[ $USE_CUDA -eq 1 ]]; then
  build_cmd+=(--features cuda)
  expected_marker="1"
fi

need_build=1
if [[ -x target/release/equium-gpu-miner ]]; then
  src_mtime=$(stat -c %Y clients/gpu-miner/src/main.rs 2>/dev/null || echo 0)
  bin_mtime=$(stat -c %Y target/release/equium-gpu-miner 2>/dev/null || echo 0)
  current_marker=$(cat "$cuda_marker" 2>/dev/null || echo "")
  if [[ "$src_mtime" -le "$bin_mtime" && "$current_marker" == "$expected_marker" ]]; then
    need_build=0
    ok "equium-gpu-miner up to date — skipping build"
  fi
fi
if [[ $need_build -eq 1 ]]; then
  spin "Building equium-gpu-miner (release, ~1–3 min)" "${build_cmd[@]}"
  mkdir -p "$(dirname "$cuda_marker")"
  printf '%s\n' "$expected_marker" > "$cuda_marker"
fi

# ----- step 3: keypair ----------------------------------------------
KEYPAIR_PATH="${EQUIUM_KEYPAIR:-$HOME/.config/solana/id.json}"
SEED_PATH="$CONFIG_DIR/seed-phrase"  # mode 600, captured at keygen
if [[ ! -f "$KEYPAIR_PATH" ]]; then
  info "Generating mining keypair at $KEYPAIR_PATH"
  mkdir -p "$(dirname "$KEYPAIR_PATH")"
  # Capture the keygen output so we can show the operator the BIP39
  # seed phrase — without it, a destroyed rental loses the wallet
  # (and any mined EQM still on it) forever.
  keygen_log=$(mktemp)
  solana-keygen new --no-bip39-passphrase --force -o "$KEYPAIR_PATH" \
    > "$keygen_log" 2>&1
  # The seed phrase is the line after "Save this seed phrase…".
  phrase=$(awk '/Save this seed phrase/{getline; while ($0 == "") getline; print; exit}' "$keygen_log")
  if [[ -n "$phrase" ]]; then
    printf '%s\n' "$phrase" > "$SEED_PATH"
    chmod 600 "$SEED_PATH"
  fi
  rm -f "$keygen_log"
  ok "Keypair created"
else
  ok "Reusing keypair at $KEYPAIR_PATH"
fi
PUBKEY=$(solana-keygen pubkey "$KEYPAIR_PATH")

# ----- wallet backup banner -----------------------------------------
# The seed phrase / keypair we just made is the *only* thing tying
# mined EQM to its owner. Print it every run so the operator can copy
# it somewhere safe before anything interrupts them. The "rental ends
# → wallet gone" framing only applies in ephemeral environments —
# Docker containers, cloud GPU rentals — so we conditional-tailor the
# warning headline + the SCP hint.
EPHEMERAL=0
if [[ $IN_DOCKER -eq 1 ]] || [[ -d /etc/vastai ]] || [[ -n "${VAST_CONTAINERLABEL:-}" ]] || [[ -n "${RUNPOD_POD_ID:-}" ]]; then
  EPHEMERAL=1
fi
hr
if [[ $EPHEMERAL -eq 1 ]]; then
  printf "${C_BOLD}SAVE THIS WALLET${C_RESET}  ${C_DIM}— rental ends → wallet gone${C_RESET}\n"
else
  printf "${C_BOLD}BACK UP THIS WALLET${C_RESET}  ${C_DIM}— store the seed phrase somewhere safe${C_RESET}\n"
fi
hr
printf "  ${C_GOLD}address${C_RESET}   %s\n" "$PUBKEY"
printf "  ${C_GOLD}keypair${C_RESET}   %s\n" "$KEYPAIR_PATH"
if [[ -s "$SEED_PATH" ]]; then
  printf "  ${C_GOLD}seed${C_RESET}      %s\n" "$(cat "$SEED_PATH")"
fi
if [[ $EPHEMERAL -eq 1 ]]; then
  printf "\n  ${C_DIM}Copy the keypair file off the rental before destroying it:${C_RESET}\n"
  printf "    ${C_MINT}scp -P <ssh-port> root@<host>:%s ~/equium-wallet.json${C_RESET}\n" "$KEYPAIR_PATH"
fi
if [[ ! -s "$SEED_PATH" ]]; then
  printf "\n  ${C_DIM}Seed phrase wasn't captured (keypair pre-existed). The keypair file is${C_RESET}\n"
  printf "  ${C_DIM}importable directly — keep ${C_RESET}%s${C_DIM} safe.${C_RESET}\n" "$KEYPAIR_PATH"
fi
hr

# ----- step 4: GPU verify (auto-probe lives in the binary) ----------
info "Verifying GPU shader (auto-probe Vulkan → GL)…"
hr
./target/release/equium-gpu-miner verify
hr

# ----- step 5: RPC URL ----------------------------------------------
# Priority: env var > saved file > interactive prompt. Save once
# entered so a reboot/rerun skips this step.
# Mainnet genesis hash — every Solana mainnet validator returns this
# from getGenesisHash, regardless of the RPC provider. We check it on
# every URL the user pastes so a devnet/testnet endpoint can't waste
# their session.
MAINNET_GENESIS="5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"

# Validate that an RPC URL responds + points at mainnet. Returns 0
# (ok), 1 (unreachable), 2 (wrong cluster). Echoes the cluster name
# we detected on stderr so the operator sees what went wrong.
validate_rpc() {
  local url="$1"
  local resp
  resp=$(curl --max-time 8 -fsSL -X POST -H 'Content-Type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"getGenesisHash"}' \
      "$url" 2>/dev/null || true)
  if [[ -z "$resp" ]]; then
    echo "no response from RPC (network / TLS / bad URL)" >&2
    return 1
  fi
  # Naive parse — we don't ship jq dependency. The genesis hash is
  # always 32-44 base58 chars inside `"result":"..."`.
  local got
  got=$(printf '%s' "$resp" | sed -n 's/.*"result":"\([^"]*\)".*/\1/p')
  if [[ -z "$got" ]]; then
    echo "RPC responded but no result field — wrong JSON-RPC dialect?" >&2
    return 1
  fi
  if [[ "$got" != "$MAINNET_GENESIS" ]]; then
    # Identify common alternatives so the message is actionable.
    case "$got" in
      EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG) echo "endpoint is devnet" >&2 ;;
      4uhcVJyU9pJkvQyS88uRDiswHXSCkY3zQawwpjk2NsNY) echo "endpoint is testnet" >&2 ;;
      *) echo "endpoint genesis ${got:0:12}… ≠ mainnet" >&2 ;;
    esac
    return 2
  fi
  return 0
}

RPC_URL="${EQUIUM_RPC_URL:-}"
if [[ -z "$RPC_URL" && -f "$RPC_FILE" ]]; then
  RPC_URL=$(<"$RPC_FILE")
  ok "Reusing saved RPC: $(printf '%.40s…' "$RPC_URL")"
fi

# Loop until we have a working + mainnet RPC URL.
while true; do
  if [[ -z "$RPC_URL" ]]; then
    printf "\n${C_BOLD}Paste your Helius (or other) mainnet RPC URL${C_RESET}\n"
    printf "${C_DIM}  Free key: https://www.helius.dev (5-minute signup)${C_RESET}\n"
    read -r -p "RPC URL: " RPC_URL
  fi
  if [[ ! "$RPC_URL" =~ ^https?:// ]]; then
    warn "RPC URL must start with http:// or https://"
    RPC_URL=""
    continue
  fi
  info "Validating RPC (cluster + reachability)…"
  case "$(validate_rpc "$RPC_URL"; echo "::$?")" in
    *::0) ok "RPC reachable + on mainnet." ; break ;;
    *::1) warn "RPC unreachable — typo? firewall? Try another URL." ; RPC_URL="" ; continue ;;
    *::2) warn "That endpoint isn't mainnet — paste your mainnet URL." ; RPC_URL="" ; continue ;;
  esac
done
printf '%s\n' "$RPC_URL" > "$RPC_FILE"
chmod 600 "$RPC_FILE"

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
# Operator can override the worker count via EQUIUM_THREADS — useful
# inside containers where num_cpus undercounts the available cores
# (vast.ai's cgroup quota often shows 8 even when the host has 64+).
# Until the CUDA Wagner kernels land, Wagner runs on CPU workers, so
# this directly bumps hashrate on under-counted hosts.
mine_args=(
  --rpc-url "$RPC_URL"
  --keypair "$KEYPAIR_PATH"
)
if [[ -n "${EQUIUM_THREADS:-}" ]]; then
  mine_args+=(--threads "$EQUIUM_THREADS")
fi

# Default to --full-gpu. All five Wagner rounds run on the GPU
# regardless of backend — CUDA on NVIDIA, wgpu/Vulkan on AMD/Intel,
# wgpu/Metal on macOS. The hybrid path (CPU Wagner + GPU BLAKE2b)
# tops out around 15 H/s and only exists as a fallback. Force it
# with EQUIUM_HYBRID=1 if the full-GPU pipeline misbehaves on your
# specific card.
if [[ -z "${EQUIUM_HYBRID:-}" ]]; then
  mine_args+=(--full-gpu)
fi

# Stash the exact mine invocation in a tiny launcher script. Two
# wins: (1) we don't have to escape array args through tmux's shell
# parser, (2) the operator can re-launch by hand later with just
# `bash ~/.config/equium/mine.sh` if they ever bypass the installer.
MINE_CMD="$CONFIG_DIR/mine.sh"
LOG_FILE="$CONFIG_DIR/mine.log"
{
  printf '#!/usr/bin/env bash\n'
  printf '# Auto-generated by %s. Edit at your own risk — re-running\n' "$SELF_CMD"
  printf '# the installer overwrites this file.\n'
  printf 'cd %q\n' "$REPO_DIR"
  printf 'exec ./target/release/equium-gpu-miner mine'
  printf ' %q' "${mine_args[@]}"
  printf '\n'
} > "$MINE_CMD"
chmod +x "$MINE_CMD"

# Already inside tmux/screen, or no tmux installed → run in the
# foreground. tmux is apt-installed earlier in this script, so the
# "no tmux" branch only fires if the apt install failed.
if [[ -n "${TMUX:-}" ]] || [[ -n "${STY:-}" ]] || ! command -v tmux >/dev/null; then
  hr
  if ! command -v tmux >/dev/null; then
    warn "tmux not installed — running in the foreground."
    warn "If your SSH connection drops, the miner stops. Install tmux"
    warn "and rerun $SELF for detach-safe mining."
  fi
  printf "${C_BOLD}Mining EQM on $PUBKEY${C_RESET}\n"
  printf "${C_DIM}Press Ctrl-C to stop. Rerun $SELF to resume.${C_RESET}\n"
  hr
  exec "$MINE_CMD"
fi

# Detach-safe launch. The miner runs inside a tmux session named
# `eqm`; we attach so the operator sees output, and Ctrl-B D detaches
# without killing anything. SSH disconnect is just an implicit detach
# — the miner keeps running until they reattach + Ctrl-C, or the box
# stops.
#
# Wrap the launcher in a one-line shell that tees stdout/stderr into
# ~/.config/equium/mine.log, so operators can grep history after the
# fact. `; exec bash` keeps the pane alive on miner exit so error
# output is readable instead of vanishing into a closed session.
hr
printf "${C_BOLD}Mining EQM on $PUBKEY${C_RESET}  ${C_DIM}— detach-safe via tmux${C_RESET}\n"
printf "${C_DIM}  Ctrl-B then D     detach without stopping the miner${C_RESET}\n"
printf "${C_DIM}  $SELF             rerun to reattach (RPC + keypair saved)${C_RESET}\n"
printf "${C_DIM}  tmux attach -t $TMUX_SESSION   reattach without the installer${C_RESET}\n"
printf "${C_DIM}  Ctrl-C            stop the miner (inside the session)${C_RESET}\n"
printf "${C_DIM}  Log:              $LOG_FILE${C_RESET}\n"
hr

tmux new-session -d -s "$TMUX_SESSION" \
  "$MINE_CMD 2>&1 | tee -a $(printf %q "$LOG_FILE"); echo; echo '[miner exited — press any key]'; read -n 1"

# Brief pause so the inner shell wires up the pty before we attach.
sleep 0.5
exec tmux attach -t "$TMUX_SESSION"
