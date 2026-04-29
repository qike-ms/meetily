#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_PLIST="${SCRIPT_DIR}/com.meetily.client.plist"
DEST_DIR="${HOME}/Library/LaunchAgents"
DEST_PLIST="${DEST_DIR}/com.meetily.client.plist"
LOG_DIR="${HOME}/Library/Logs"
BIN_PATH="${MEETILY_CLIENT_BIN:-${HOME}/git/meetily/target/release/meetily-client}"

if [[ ! -x "${BIN_PATH}" ]]; then
  cat >&2 <<EOF
meetily-client binary was not found or is not executable:
  ${BIN_PATH}

Build it first, for example:
  cargo build --release --bin meetily-client

Or set MEETILY_CLIENT_BIN to the built binary path before running this installer.
EOF
  exit 1
fi

mkdir -p "${DEST_DIR}" "${LOG_DIR}"
sed "s#__HOME__#${HOME}#g" "${SOURCE_PLIST}" > "${DEST_PLIST}"
chmod 644 "${DEST_PLIST}"

launchctl bootout "gui/$(id -u)" "${DEST_PLIST}" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$(id -u)" "${DEST_PLIST}"
launchctl enable "gui/$(id -u)/com.meetily.client"

echo "Installed ${DEST_PLIST}"
echo "Start: launchctl kickstart gui/$(id -u)/com.meetily.client"
echo "Stop:  launchctl bootout gui/$(id -u) ${DEST_PLIST}"
