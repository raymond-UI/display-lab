#!/usr/bin/env bash
# Grant Screen Recording to the given executables and to every process above
# this script, by writing the TCC databases directly.
#
# This is for disposable CI machines only. Apple documents TCC's on-disk
# format as an implementation detail; this can break with any macOS update.
# It needs root and a writable system TCC database (true on GitHub's hosted
# macOS images today).
set -euo pipefail

SYSTEM_DB="/Library/Application Support/com.apple.TCC/TCC.db"
USER_DB="$HOME/Library/Application Support/com.apple.TCC/TCC.db"

clients=("$@")
# TCC may attribute the request to a "responsible" ancestor rather than the
# binary itself, so grant the whole chain above us too.
pid=$$
while [ "$pid" -gt 1 ]; do
  exe=$(ps -o comm= -p "$pid" | sed 's/^-//' || true)
  if [ -n "$exe" ]; then
    case "$exe" in
      /*) clients+=("$exe") ;;
      *) path=$(command -v "$exe" 2>/dev/null || true); [ -n "$path" ] && clients+=("$path") ;;
    esac
  fi
  pid=$(ps -o ppid= -p "$pid" | tr -d ' ')
done

grant() {
  local db=$1 client=$2
  [ -f "$db" ] || return 0
  sudo sqlite3 "$db" "INSERT OR REPLACE INTO access
    (service, client, client_type, auth_value, auth_reason, auth_version, indirect_object_identifier, flags, last_modified)
    VALUES ('kTCCServiceScreenCapture', '$client', 1, 2, 4, 1, 'UNUSED', 0, CAST(strftime('%s','now') AS INTEGER));" \
    && echo "granted $client in $(basename "$(dirname "$db")")/$(basename "$db")" \
    || echo "could not write $db for $client"
}

for c in $(printf '%s\n' "${clients[@]}" | sort -u); do
  grant "$SYSTEM_DB" "$c"
  grant "$USER_DB" "$c"
done

echo "Screen Recording rows now:"
sudo sqlite3 "$SYSTEM_DB" "SELECT client, auth_value FROM access WHERE service='kTCCServiceScreenCapture';" || true
