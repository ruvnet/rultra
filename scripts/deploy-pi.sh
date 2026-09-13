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
BINARIES="rultra rultra-sense rultra-ui rultra-spatial rultra-mcp"

echo "── building ──"
./scripts/build-pi.sh --features hardware $(printf -- '-p %s ' $BINARIES)

echo "── shipping to $HOST ──"
for b in $BINARIES; do
  [ -f "$BIN/$b" ] || { echo "missing $BIN/$b"; exit 1; }
  scp -q "$BIN/$b" "$HOST:/tmp/$b"
done
scp -q deploy/rultra-ui.service "$HOST:/tmp/rultra-ui.service"
scp -q deploy/desktop/rultra-console "$HOST:/tmp/rultra-console"
scp -q deploy/desktop/rultra-console.desktop "$HOST:/tmp/rultra-console.desktop"
scp -q deploy/desktop/rultra-console.svg "$HOST:/tmp/rultra-console.svg"

ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
for b in rultra rultra-sense rultra-ui rultra-spatial rultra-mcp; do
  sudo install -m0755 "/tmp/$b" "/usr/local/bin/$b"
done
sudo install -m0644 /tmp/rultra-ui.service /etc/systemd/system/rultra-ui.service
sudo mkdir -p /etc/rultra /var/lib/rultra

# Desktop application: launcher, menu entry, icon.
sudo install -m0755 /tmp/rultra-console /usr/local/bin/rultra-console
sudo install -d /usr/local/share/applications /usr/local/share/icons/hicolor/scalable/apps
sudo install -m0644 /tmp/rultra-console.desktop /usr/local/share/applications/rultra-console.desktop
sudo install -m0644 /tmp/rultra-console.svg \
  /usr/local/share/icons/hicolor/scalable/apps/rultra-console.svg
sudo update-desktop-database /usr/local/share/applications 2>/dev/null || true
sudo gtk-update-icon-cache -f -t /usr/local/share/icons/hicolor 2>/dev/null || true

# Generate the console token on first deploy only. It is never printed and
# never leaves the box; read it from /etc/rultra/ui.env when you need it.
if ! sudo test -s /etc/rultra/ui.env; then
  # Two INDEPENDENT tokens, not one derived from the other: a read-only
  # credential computed from the control credential is one bug away from being
  # a control credential.
  TOK=$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | cut -c1-24)
  ROTOK=$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | cut -c1-24)
  printf 'RULTRA_UI_BIND=0.0.0.0\nRULTRA_UI_PORT=17880\nRULTRA_UI_TOKEN=%s\nRULTRA_UI_READ_TOKEN=%s\nRULTRA_UI_LOCAL_LISTEN=1\n' \
    "$TOK" "$ROTOK" | sudo tee /etc/rultra/ui.env >/dev/null
  sudo chmod 600 /etc/rultra/ui.env
  echo "generated control + read-only console tokens at /etc/rultra/ui.env (0600, not printed)"
fi

# The desktop app renders telemetry with no credential, which requires the
# loopback read grant. Added to an existing env file rather than assumed,
# because the grant is off by default in the binary on purpose.
if ! sudo grep -q RULTRA_UI_LOCAL_LISTEN /etc/rultra/ui.env; then
  printf 'RULTRA_UI_LOCAL_LISTEN=1\n' | sudo tee -a /etc/rultra/ui.env >/dev/null
  echo "enabled the loopback read grant for the desktop app"
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
SHELL_CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 6 http://127.0.0.1:17880/)
echo "  shell:    HTTP $SHELL_CODE (want 200)"
[ "$SHELL_CODE" = "200" ] || { echo "  FAILED: console shell not served"; exit 1; }

# NOTE: unauthenticated *loopback* reads are allowed on purpose — that is the
# desktop app's grant — so asserting 401 from here would contradict the design.
# Whether the box is exposed to the NETWORK is a different question and can
# only be answered from off the box; the deploying host checks it below.

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

# The desktop app must be able to read WITHOUT a token, and must still be
# unable to write. Asserting both, because a grant that silently conveys
# control would be worse than no grant.
LOCAL_READ=$(curl -s -o /dev/null -w '%{http_code}' --max-time 6 http://127.0.0.1:17880/api/summary)
LOCAL_WRITE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 6 -X POST http://127.0.0.1:17880/api/cycle)
echo "  desktop:  local GET $LOCAL_READ (want 200) · local POST $LOCAL_WRITE (want 401)"
[ "$LOCAL_READ" = "200" ]  || { echo "  FAILED: desktop app cannot read locally"; exit 1; }
[ "$LOCAL_WRITE" = "401" ] || { echo "  FAILED: loopback grant conveys WRITE access"; exit 1; }

for b in rultra rultra-sense rultra-spatial rultra-mcp; do
  printf '  %-15s %s\n' "$b" "$(command -v $b || echo -)"
done
printf '  %-15s %s\n' "desktop entry" "$(test -f /usr/local/share/applications/rultra-console.desktop && echo installed || echo MISSING)"
REMOTE

# Exposure is a property of the network, not of the box, so it is asserted from
# here — the deploying host, which is remote to the Pi. This is the check the
# in-box one could never make honestly once loopback reads were granted.
REMOTE_HOST="${HOST#*@}"
echo "── verifying from off the box ──"
R_READ=$(curl -s -o /dev/null -w '%{http_code}' --max-time 8 "http://$REMOTE_HOST:17880/api/summary" || echo 000)
R_WRITE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 8 -X POST "http://$REMOTE_HOST:17880/api/cycle" || echo 000)
echo "  remote:   GET $R_READ (want 401) · POST $R_WRITE (want 401)"
[ "$R_READ" = "401" ]  || { echo "  FAILED: the network can READ without a token"; exit 1; }
[ "$R_WRITE" = "401" ] || { echo "  FAILED: the network can WRITE without a token"; exit 1; }
echo "── done ──"
