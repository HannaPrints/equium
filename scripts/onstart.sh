#!/usr/bin/env bash
#
# vast.ai template on-start hook.
#
# The template editor in vast.ai's UI line-wraps long commands into
# literal newlines, which silently breaks any multi-line on-start
# script. So our template's on-start is a single line that just
# downloads + runs this file, which has no length constraint:
#
#   curl -fsSL https://raw.githubusercontent.com/HannaPrints/equium/master/scripts/onstart.sh | bash
#
# What this does:
#   1. Waits up to ~30s for DNS + HTTPS to come up (containers
#      occasionally start before networking).
#   2. Stages the bootstrap script at /root/cloud-mine.sh.
#   3. Drops an MOTD so the operator sees the next command on SSH login.
#
# Designed to fail-soft. If anything below explodes we still want the
# operator to land in a usable shell — the manual install path in
# /download (curl + chmod + ./cloud-mine.sh) is always available.

set +e  # explicitly NOT `set -e` — we want to limp through partial failures

log() { printf '[onstart] %s\n' "$*" | tee -a /var/log/equium-onstart.log; }

log "starting at $(date -u +%FT%TZ)"

# Wait for network. New cloud containers sometimes route DNS before
# they route HTTPS, so curl can transiently fail at boot.
for i in 1 2 3 4 5 6 7 8 9 10; do
  if curl -fsSL --max-time 5 https://raw.githubusercontent.com -o /dev/null 2>/dev/null; then
    log "network ready after ${i} attempts"
    break
  fi
  sleep 3
done

# Stage the bootstrap script. If this fails, the operator can still
# do it manually via the /download instructions.
if curl -fsSL https://raw.githubusercontent.com/HannaPrints/equium/master/scripts/cloud-mine.sh \
     -o /root/cloud-mine.sh; then
  chmod +x /root/cloud-mine.sh
  log "cloud-mine.sh staged at /root/cloud-mine.sh"
else
  log "WARN: could not download cloud-mine.sh — fall back to /download manual instructions"
fi

# Drop a MOTD hint. Single-quoted heredoc keeps it free of shell
# substitution surprises in this context.
cat > /etc/motd << 'MOTD_END'

  ═══════════════════════════════════════════════════════
  Equium ($EQM) miner — vast.ai template
  ═══════════════════════════════════════════════════════

  Run:  ./cloud-mine.sh

  It will install Rust + Vulkan + Solana CLI, build the miner,
  prompt for a Helius RPC URL, then start mining.

  If cloud-mine.sh is missing for any reason, install it with:
    curl -fsSL https://raw.githubusercontent.com/HannaPrints/equium/master/scripts/cloud-mine.sh \
      -o cloud-mine.sh && chmod +x cloud-mine.sh

  ═══════════════════════════════════════════════════════

MOTD_END

log "done"
