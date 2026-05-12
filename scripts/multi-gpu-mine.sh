#!/usr/bin/env bash
# multi-gpu-mine.sh — launch one equium-gpu-miner process per GPU.
#
# Usage:
#   ./scripts/multi-gpu-mine.sh --rpc-url <URL> --keypair <PATH> [--full-gpu]
#
# Each GPU gets its own tmux window and log file under ~/.equium/logs/.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MINER="${SCRIPT_DIR}/../target/release/equium-gpu-miner"

if [[ ! -x "$MINER" ]]; then
  echo "Error: miner binary not found at $MINER"
  echo "Run: cargo build --release -p equium-gpu-miner [--features cuda]"
  exit 1
fi

# Detect number of GPUs
GPU_COUNT=$(nvidia-smi --list-gpus 2>/dev/null | wc -l)
if [[ "$GPU_COUNT" -eq 0 ]]; then
  echo "Error: no NVIDIA GPUs detected"
  exit 1
fi

echo "Detected $GPU_COUNT GPU(s)"

LOG_DIR="$HOME/.equium/logs"
mkdir -p "$LOG_DIR"

# Check tmux
if ! command -v tmux >/dev/null; then
  echo "Error: tmux is required. Install with: apt install -y tmux"
  exit 1
fi

SESSION="equium-multi"
tmux new-session -d -s "$SESSION" 2>/dev/null || true

for i in $(seq 0 $((GPU_COUNT - 1))); do
  GPU_NAME=$(nvidia-smi --query-gpu=name --format=csv,noheader --id="$i" 2>/dev/null || echo "GPU $i")
  LOG_FILE="$LOG_DIR/gpu-${i}.log"

  echo "Starting miner on GPU $i ($GPU_NAME) → log: $LOG_FILE"

  WINDOW="${SESSION}:gpu-${i}"
  tmux new-window -t "$SESSION" -n "gpu-${i}" 2>/dev/null || true

  tmux send-keys -t "$WINDOW" \
    "CUDA_VISIBLE_DEVICES=$i EQUIUM_GPU_ID=$i $MINER $* 2>&1 | tee $LOG_FILE" \
    Enter
done

echo ""
echo "All $GPU_COUNT miners started in tmux session '$SESSION'"
echo "Attach with:  tmux attach -t $SESSION"
echo "Switch GPU:   Ctrl+B, n (next window)"
echo "Detach:       Ctrl+B, d"
echo "Stop all:     tmux kill-session -t $SESSION"
