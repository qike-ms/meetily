#!/usr/bin/env bash
set -euo pipefail

MODEL_NAME="${1:-large-v3-turbo}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
BIN_PATH="${MEETILY_CLIENT_BIN:-${TARGET_DIR}/release/meetily-client}"

if [[ ! -x "${BIN_PATH}" ]]; then
  cat >&2 <<EOF
meetily-client binary was not found or is not executable:
  ${BIN_PATH}

Build it first, for example:
  cargo build --release --bin meetily-client

Or set MEETILY_CLIENT_BIN to the built binary path.
EOF
  exit 1
fi

exec "${BIN_PATH}" download-model "${MODEL_NAME}"
