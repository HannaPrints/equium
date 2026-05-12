#!/usr/bin/env bash
#
# Back-compat shim. The canonical installer is now `install.sh`. This
# file exists so:
#   1. Old vast.ai templates whose on-start scripts still curl
#      `cloud-mine.sh` keep working without our intervention.
#   2. Anyone who saved the previous curl one-liner from /download
#      keeps having a working command after pull.
#
# Forwards all args + exit code to install.sh. New users should go
# straight to scripts/install.sh.

exec "$(dirname "$0")/install.sh" "$@"
