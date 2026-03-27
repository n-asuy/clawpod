#!/usr/bin/env bash
set -euo pipefail

: "${DEPLOY_HOST:?Set DEPLOY_HOST (e.g. 188.245.67.107)}"
: "${DEPLOY_USER:=root}"
: "${DEPLOY_SRC_DIR:=/opt/clawpod-src}"

BIN_PATH="/usr/local/bin/clawpod"
SERVICE="clawpod"
BRANCH="${1:-main}"

echo "==> Deploying branch '${BRANCH}' to ${DEPLOY_USER}@${DEPLOY_HOST}"

ssh "${DEPLOY_USER}@${DEPLOY_HOST}" bash -s "${BRANCH}" "${DEPLOY_SRC_DIR}" "${BIN_PATH}" "${SERVICE}" <<'REMOTE'
set -euo pipefail
source "$HOME/.cargo/env" 2>/dev/null || true
BRANCH="$1"
SRC_DIR="$2"
BIN_PATH="$3"
SERVICE="$4"

echo "--- git pull (${BRANCH})"
cd "${SRC_DIR}"
git fetch origin
git checkout "${BRANCH}"
git reset --hard "origin/${BRANCH}"

echo "--- cargo build --release -p runtime"
cargo build --release -p runtime

echo "--- restart service"
systemctl stop "${SERVICE}"
cp target/release/clawpod "${BIN_PATH}"
chmod +x "${BIN_PATH}"
systemctl start "${SERVICE}"

sleep 2
if systemctl is-active --quiet "${SERVICE}"; then
  echo "==> deploy ok: $(${BIN_PATH} --version 2>/dev/null || echo 'running')"
else
  echo "==> ERROR: service failed to start"
  journalctl -u "${SERVICE}" --no-pager -n 20
  exit 1
fi
REMOTE
