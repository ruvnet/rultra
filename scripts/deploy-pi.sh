#!/usr/bin/env bash
# Build and install rultra on a Raspberry Pi.
#
#   scripts/deploy-pi.sh [user@host]        (default: pi@raspberrypi)
#
# Deploys ALL binaries together, on purpose. Deploying them piecemeal is how the
# box ends up running a mix of versions: the console reported a field the
# installed `rultra` binary did not yet emit, because only the console had been
# refreshed. One command, one consistent set.
#
# Builds through scripts/build-pi.sh, which uses a bookworm container so the
# sysroot glibc matches Raspberry Pi OS. See that script for why.
set -euo pipefail
HOST="${1:-pi@raspberrypi}"
cd "$(dirname "$0")/.."
BIN=target-bookworm/aarch64-unknown-linux-gnu/release
BINARIES="rultra rultra-sense rultra-ui"

echo "── building ──"
./scripts/build-pi.sh --features hardware $(printf -- '-p %s ' $BINARIES)

echo "── shipping to $HOST ──"
for b in $BINARIES; do
  [ -f "$BIN/$b" ] || { echo "missing $BIN/$b"; exit 1; }
  scp -q "$BIN/$b" "$HOST:/tmp/$b"
done
scp -q deploy/rultra-ui.service "$HOST:/tmp/rultra-ui.service"

ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
for b in rultra rultra-sense rultra-ui; do
  sudo install -m0755 "/tmp/$b" "/usr/local/bin/$b"
done
sudo install -m0644 /tmp/rultra-ui.service /etc/systemd/system/rultra-ui.service
sudo mkdir -p /etc/rultra /var/lib/rultra

# Generate the console token on first deploy only. It is never printed and
# never leaves the box; read it from /etc/rultra/ui.env when you need it.
if ! sudo test -s /etc/rultra/ui.env; then
  # Two INDEPENDENT tokens, not one derived from the other: a read-only
  # credential computed from the control credential is one bug away from being
  # a control credential.
  TOK=$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | cut -c1-24)
  ROTOK=$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | cut -c1-24)
  printf 'RULTRA_UI_BIND=0.0.0.0\nRULTRA_UI_PORT=17880\nRULTRA_UI_TOKEN=%s\nRULTRA_UI_READ_TOKEN=%s\n' \
    "$TOK" "$ROTOK" | sudo tee /etc/rultra/ui.env >/dev/null
  sudo chmod 600 /etc/rultra/ui.env
  echo "generated control + read-only console tokens at /etc/rultra/ui.env (0600, not printed)"
fi

sudo systemctl daemon-reload
# enable and restart are SEPARATE steps on purpose. `enable --now` succeeds
# without doing anything when the unit is already enabled and running, so
# chaining a restart behind `||` means an upgrade never actually loads the new
# binary — and the deploy still reports success. That happened.
sudo systemctl enable rultra-ui >/dev/null 2>&1 || true
sudo systemctl restart rultra-ui
sleep 3

echo "── verifying ──"
echo "  service:  $(systemctl is-active rultra-ui)"
# The shell must be reachable and the API must NOT be, without a token. If the
# second check ever returns 200, the box is exposed and this deploy has failed.
SHELL_CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 6 http://127.0.0.1:17880/)
API_CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 6 http://127.0.0.1:17880/api/summary)
echo "  shell:    HTTP $SHELL_CODE (want 200)"
echo "  api/auth: HTTP $API_CODE (want 401)"
[ "$SHELL_CODE" = "200" ] || { echo "  FAILED: console shell not served"; exit 1; }
[ "$API_CODE" = "401" ] || { echo "  FAILED: API answered without a token"; exit 1; }

# Assert the capability split is real, not just configured. A read-only token
# that can run a cycle is worse than no split at all, because it is trusted.
RO=$(sudo grep -oP 'RULTRA_UI_READ_TOKEN=\K.*' /etc/rultra/ui.env 2>/dev/null || true)
if [ -n "$RO" ]; then
  RO_READ=$(curl -s -o /dev/null -w '%{http_code}' --max-time 6 \
    -H "Authorization: Bearer $RO" http://127.0.0.1:17880/api/summary)
  RO_WRITE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 6 -X POST \
    -H "Authorization: Bearer $RO" http://127.0.0.1:17880/api/cycle)
  echo "  read-only: GET $RO_READ (want 200) · POST $RO_WRITE (want 401)"
  [ "$RO_READ" = "200" ] || { echo "  FAILED: read-only token cannot read"; exit 1; }
  [ "$RO_WRITE" = "401" ] || { echo "  FAILED: read-only token can WRITE"; exit 1; }
fi

# Assert the RUNNING process is the binary we just installed. A restart that
# silently did not happen is the failure this catches; comparing inodes is the
# only check that cannot be fooled by a successful-looking systemctl call.
RUNNING_INODE=$(sudo stat -L -c %i "/proc/$(systemctl show -p MainPID --value rultra-ui)/exe" 2>/dev/null || echo none)
INSTALLED_INODE=$(stat -c %i /usr/local/bin/rultra-ui)
if [ "$RUNNING_INODE" = "$INSTALLED_INODE" ]; then
  echo "  running:  the binary just installed"
else
  echo "  FAILED: rultra-ui is running a different binary than the one installed"
  exit 1
fi

for b in rultra rultra-sense; do
  printf '  %-13s %s\n' "$b" "$(command -v $b)"
done
REMOTE
echo "── done ──"
