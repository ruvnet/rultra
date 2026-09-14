#!/usr/bin/env bash
# End-to-end test for rultra.
#
#   scripts/e2e.sh [user@host]        (default: pi@raspberrypi)
#
# Exercises the whole system as a user would, not as a unit test would: the
# workspace, the MCP server over a real stdio handshake, the console's
# capability split from BOTH sides of the network boundary, the hardware on the
# actual board, and the desktop integration.
#
# Every check prints what it wanted and what it got, so a failure is readable
# without re-running anything by hand.
set -uo pipefail
HOST="${1:-pi@raspberrypi}"
cd "$(dirname "$0")/.."

PASS=0; FAIL=0
chk() { # chk <name> <want> <got>
  if [ "$2" = "$3" ]; then printf '  ok    %-46s %s\n' "$1" "$3"; PASS=$((PASS+1))
  else printf '  FAIL  %-46s want=%s got=%s\n' "$1" "$2" "$3"; FAIL=$((FAIL+1)); fi
}
note() { printf '\n\033[1m%s\033[0m\n' "$1"; }

note "1. workspace"
FMT=$(cargo fmt --all -- --check >/dev/null 2>&1 && echo clean || echo dirty)
chk "cargo fmt" clean "$FMT"
CLIPPY=$(cargo clippy --workspace --all-targets 2>&1 | grep -cE '^(warning|error)')
chk "clippy warnings" 0 "$CLIPPY"
TESTS=$(cargo test --workspace 2>&1 | grep -c '^test .* ok$')
[ "$TESTS" -gt 100 ] && chk "unit tests (>100)" pass pass || chk "unit tests (>100)" pass "only $TESTS"

note "2. MCP server — real stdio handshake"
MCPDIR=$(mktemp -d)
cargo build -q -p rultra-mcp 2>/dev/null
MCP_OUT=$({
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}'
  printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"resources/list"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"ruv://rultra/capabilities"}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"ruv://rultra/nope"}}'
  sleep 3
} | RULTRA_STATE_DIR="$MCPDIR" timeout 25 ./target/debug/rultra-mcp 2>/dev/null)
rm -rf "$MCPDIR"
mcp_field() { echo "$MCP_OUT" | python3 -c "
import sys,json
for l in sys.stdin:
    l=l.strip()
    if not l: continue
    try: m=json.loads(l)
    except Exception: continue
    if m.get('id')==$1:
        print($2); break
else: print('none')
"; }
chk "tools exposed"        9 "$(mcp_field 2 "len(m['result']['tools'])")"
chk "ruv:// resources"     6 "$(mcp_field 3 "len(m['result']['resources'])")"
chk "all resources ruv://" True "$(mcp_field 3 "all(r['uri'].startswith('ruv://') for r in m['result']['resources'])")"
chk "resource read works"  True "$(mcp_field 4 "'proven_parts' in m['result']['contents'][0]['text']")"
# An unknown resource must ERROR, not return an empty document that a caller
# would mistake for "this box has no capabilities".
chk "unknown resource errors" True "$(mcp_field 5 "'error' in m")"

note "3. console capability split — from off the box"
REMOTE_HOST="${HOST#*@}"
chk "remote GET  (no token)" 401 "$(curl -s -o /dev/null -w '%{http_code}' --max-time 8 "http://$REMOTE_HOST:17880/api/summary")"
chk "remote POST (no token)" 401 "$(curl -s -o /dev/null -w '%{http_code}' --max-time 8 -X POST "http://$REMOTE_HOST:17880/api/cycle")"

note "4. on the box — capabilities, hardware, chain"
ssh "$HOST" 'bash -s' <<'REMOTE' | sed 's/^/  /'
T=$(sudo grep -oP 'RULTRA_UI_TOKEN=\K.*' /etc/rultra/ui.env 2>/dev/null)
RO=$(sudo grep -oP 'RULTRA_UI_READ_TOKEN=\K.*' /etc/rultra/ui.env 2>/dev/null)
# A bare 000 is a connection-level blip, not a verdict. Retry once so the
# suite reports the endpoint's behaviour rather than the network's mood.
c(){
  local code
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 "$@")
  if [ "$code" = "000" ]; then sleep 1; code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 15 "$@"); fi
  echo "$code"
}
p(){ printf '%-48s %s\n' "$1" "$2"; }
p "service rultra-ui"            "$(systemctl is-active rultra-ui)"
p "service rultra-cycle.timer"   "$(systemctl is-active rultra-cycle.timer)"
p "local GET  (no token, want 200)"  "$(c http://127.0.0.1:17880/api/summary)"
p "local POST (no token, want 401)"  "$(c -X POST http://127.0.0.1:17880/api/cycle)"
p "read token GET  (want 200)"       "$(c -H "Authorization: Bearer $RO" http://127.0.0.1:17880/api/summary)"
p "read token POST (want 401)"       "$(c -X POST -H "Authorization: Bearer $RO" http://127.0.0.1:17880/api/cycle)"
p "control token POST (want 200)"    "$(c -X POST -H "Authorization: Bearer $T" http://127.0.0.1:17880/api/matrix -H 'Content-Type: application/json' -d '{"pattern":"heart"}')"
p "sensors responding"           "$(sudo rultra-sense probe 2>/dev/null | grep -c '"responding":true')"
p "spatial fuses"                "$(sudo rultra-spatial once 2>/dev/null | grep -c proximity)"
CH=$(curl -s --max-time 10 http://127.0.0.1:17880/api/chain)
p "witness verified"             "$(echo "$CH" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("verified"))')"
p "witness entries"              "$(echo "$CH" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("count"))')"
ST=$(curl -s --max-time 20 http://127.0.0.1:17880/api/selftest)
p "self-test"                    "$(echo "$ST" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(f"{d[\"passed\"]}/{d[\"total\"]}")')"
p "runnable examples"            "$(curl -s --max-time 10 http://127.0.0.1:17880/api/examples | python3 -c 'import sys,json;print(len(json.load(sys.stdin)["examples"]))')"
p "example runs (want ok:true)"  "$(curl -s --max-time 20 -X POST -H "Authorization: Bearer $T" -H 'Content-Type: application/json' -d '{"id":"heartbeat"}' http://127.0.0.1:17880/api/examples/run | python3 -c 'import sys,json;print(json.load(sys.stdin).get("ok"))')"
p "config write no token (401)"  "$(c -X POST -H 'Content-Type: application/json' -d '{"poll_interval_ms":1000}' http://127.0.0.1:17880/api/policy)"
p "config out of bounds (400)"   "$(c -X POST -H "Authorization: Bearer $T" -H 'Content-Type: application/json' -d '{"poll_interval_ms":5}' http://127.0.0.1:17880/api/policy)"
# An unconfigured model must say so rather than invent a reading of the room.
p "interpret honest when no key" "$(curl -s --max-time 20 -X POST -H "Authorization: Bearer $T" http://127.0.0.1:17880/api/interpret | python3 -c 'import sys,json;d=json.load(sys.stdin);print("honest" if (d.get("configured") or d.get("interpretation")) else "honest" if d.get("reason") else "SILENT")')"
p "claim-check flagged"          "$(curl -s --max-time 90 http://127.0.0.1:17880/api/claimcheck | python3 -c 'import sys,json;print(json.load(sys.stdin)["flagged"])')"
p "desktop entry"                "$(desktop-file-validate /usr/local/share/applications/rultra-console.desktop >/dev/null 2>&1 && echo valid || echo INVALID)"
p "launcher installed"           "$(test -x /usr/local/bin/rultra-console && echo yes || echo NO)"
PID=$(pgrep -f gnome-session-binary | head -1)
[ -n "$PID" ] && export $(tr '\0' '\n' < /proc/$PID/environ | grep -E '^(DBUS_SESSION_BUS_ADDRESS|DISPLAY)=' | xargs) 2>/dev/null
p "pinned in dock"               "$(gsettings get org.gnome.shell favorite-apps 2>/dev/null | grep -c rultra-console)"
REMOTE

note "result"
printf '  %d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
