#!/usr/bin/env bash
# Does a focused Roblox TextBox get an editor, in the right place, with the
# characters in it?
#
# This exists because the answer changed three times in one session and each
# time the evidence was a hand-driven run that was hard to repeat. Typing into a
# box is a five-step dance -- launch, wait for the app shell, click the box,
# type, photograph the composited result -- and four of those five steps have
# their own way of silently not happening:
#
#   * fixed `sleep`s race the app shell. Four runs were read as "the click
#     missed the button" when the page simply had not rendered yet. This polls
#     the log for readiness instead.
#   * `CORDIAL_SCRIPT` parses and never fires unless `CORDIAL_INSTR=1` is also
#     set. That cost four more runs.
#   * `cordial_screenshot` reads the engine's own swapchain and cannot see a GTK
#     editor at all, so it reports success whatever happens. A composited
#     screenshot (grim under sway, ScreenShot2 under KWin) against the
#     nested compositor is the only capture that proves anything here.
#   * a client left over from a previous run holds the profile lock, and the
#     refusal reads as a launch failure.
#
# What it asserts, in order, so a failure says which step broke:
#
#   1. the app shell reaches a screen that has a search box
#   2. clicking it focuses a TextBox
#   3. `showKeyboard` delivers a NativeTextBoxInfo, and the editor ends up
#      somewhere with a real size -- on the box where the engine says where it
#      is, in the fallback bar where it does not
#   4. the characters reach the engine
#   5. a composited screenshot exists to look at
#
# Step 3 is the one worth having. It was null for the entire life of this
# project because the `<init>` hook was one argument short of the dex signature,
# and nothing noticed because a null spec and a spec nobody asked for look
# identical from outside.
#
# Usage:  tools/text-entry-check.sh [profile] [text]
# Needs:  a signed-in profile, a headless compositor this runner can start,
#         and a build. `CORDIAL_HEADLESS_COMPOSITOR=kwin` uses KWin's virtual
#         backend; sway is the default where it is installed.

set -uo pipefail

PROFILE="${1:-CordialTest}"
TYPED="${2:-rivals}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${CORDIAL_CHECK_OUT:-/tmp/cordial-text-entry-check}"
mkdir -p "$OUT"
LOG="$OUT/client.log"
SHOT="$OUT/typed.png"
BIN="$ROOT/target/release/cordial-run"
COMP="$OUT/compositor"
STATE="$COMP/state.json"

APK="${CORDIAL_APK:-$HOME/.var/app/org.vinegarhq.Sober/data/sober/packages/x86_64/com.roblox.client/base.apk}"
LIB="${CORDIAL_LIB_DIR:-$HOME/.cache/cordial/lib/x86_64}"

fail() { echo "FAIL: $*"; exit 1; }
note() { echo "  $*"; }

[ -x "$BIN" ] || fail "no build at $BIN -- cargo build --release first"
[ -f "$APK" ] || fail "no APK at $APK"

# Never match our own command line; the engine renames its main thread to
# `Main`, so `pgrep -x cordial-run` finds nothing either.
reap() {
  for p in $(pidof cordial-run 2>/dev/null); do
    prof=$(tr '\0' '\n' < "/proc/$p/cmdline" 2>/dev/null | grep -A1 -x -- --profile | tail -1)
    [ "$prof" = "$PROFILE" ] && kill -9 "$p" 2>/dev/null
  done
  sleep 2
}

echo "== text entry check: profile=$PROFILE typing=$TYPED"
reap
: > "$LOG"

python3 "$ROOT/tools/headless_compositor.py" stop --state "$STATE" >/dev/null 2>&1 || true
python3 "$ROOT/tools/headless_compositor.py" start --state "$STATE" \
  --out-dir "$COMP" --width 1280 --height 800 --require screenshot >/dev/null ||
  fail "could not start a headless compositor with screenshot support"
eval "$(python3 "$ROOT/tools/headless_compositor.py" env --state "$STATE")"
trap 'python3 "$ROOT/tools/headless_compositor.py" stop --state "$STATE" >/dev/null 2>&1 || true' EXIT

nohup env CORDIAL_DEV_CONTROL=1 CORDIAL_TRACE_TEXT=1 CORDIAL_INSTR=1 \
  ${CORDIAL_KEYS_TO_GAME_WHILE_TYPING:+CORDIAL_KEYS_TO_GAME_WHILE_TYPING="$CORDIAL_KEYS_TO_GAME_WHILE_TYPING"} \
  python3 "$ROOT/tools/headless_compositor.py" exec --state "$STATE" -- \
  "$BIN" --lib-dir "$LIB" --apk "$APK" \
    --host-libc --game-activity --run 0 --profile "$PROFILE" > "$LOG" 2>&1 &

# 1. wait for the app shell rather than guessing at it
for _ in $(seq 1 90); do
  grep -q 'app ready: \(Home\|Landing\)' "$LOG" && break
  sleep 1
done
grep -q 'app ready: \(Home\|Landing\)' "$LOG" || { reap; fail "app shell never became ready (step 1)"; }
note "step 1 ok: $(grep -oE 'app ready: [A-Za-z]+' "$LOG" | tail -1)"
# The shell reports ready before it has painted; this is the one wait that has
# to be a sleep, because nothing in the log marks "laid out".
sleep 14

SOCK="$HOME/.local/share/cordial/profiles/$PROFILE/devctl.sock"
mcp() {
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' "$1" \
    | timeout 30 python3 "$ROOT/tools/cordial-mcp.py" --socket "$SOCK" 2>/dev/null | tail -1
}

# 2. click the search box, and keep trying. "Ready" is not "laid out", and a
#    single click at a fixed coordinate is the step that flaked most often --
#    every failure of it was misread as a broken fix at least once today.
shoot() {
  timeout 25 python3 "$ROOT/tools/headless_compositor.py" screenshot \
    --state "$STATE" --path "$1" >/dev/null 2>&1
}
shoot "$OUT/before-click.png"

for attempt in 1 2 3 4 5; do
  mcp '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"cordial_click","arguments":{"x":674,"y":28}}}' >/dev/null
  sleep 4
  grep -q 'textbox focused' "$LOG" && break
  note "step 2: attempt $attempt found no focused TextBox, retrying"
  sleep 4
done
if ! grep -q 'textbox focused' "$LOG"; then
  shoot "$OUT/no-focus.png"
  reap
  fail "clicking the search box focused no TextBox after 5 tries (step 2) -- see $OUT/no-focus.png for what was on screen"
fi
note "step 2 ok: a TextBox took focus"

# 3. the geometry. Null was the year-long bug; all-zero is a real answer the
#    engine gives for some boxes and is not a pass.
#
#    Two readings, because they are two different facts and conflating them
#    cost a run. `textbox spec from showKeyboard` is what the engine
#    volunteered at focus time, and for the search modal it is legitimately
#    zeroed -- the modal has not laid out yet. `text editor placed from` is
#    where the editor actually went, after `sync_text_overlay` has consulted
#    `nativeGetTextBoxInfo` and, failing that, its own fallback bar. The second
#    is the one that decides whether typing is usable, so it is the one the
#    warning hangs on.
spec=$(grep -oE 'textbox spec from showKeyboard x=[-0-9.]+ y=[-0-9.]+ w=[-0-9.]+ h=[-0-9.]+' "$LOG" | tail -1)
[ -n "$spec" ] || { reap; fail "no NativeTextBoxInfo arrived -- showKeyboard got null (step 3)"; }
note "step 3 ok: $spec"
placed=$(grep -oE 'text editor placed from [a-z ]+ x=[-0-9.]+ y=[-0-9.]+ w=[-0-9.]+ h=[-0-9.]+' "$LOG" | tail -1)
[ -n "$placed" ] || { reap; fail "the editor was never placed at all (step 3)"; }
w=$(sed -E 's/.* w=([-0-9.]+) .*/\1/' <<<"$placed")
h=$(sed -E 's/.* h=([-0-9.]+)$/\1/' <<<"$placed")
note "        $placed"
case "$placed" in
  *"from engine geometry"*)
    note "        placed on the box from the engine's own geometry" ;;
  *)
    note "        WARNING: no geometry for this box; the editor is a placed bar" ;;
esac
awk "BEGIN{exit !($w > 0 && $h > 0)}" ||
  { reap; fail "the editor was placed 0-sized and is invisible (step 3)"; }

# 4. type, per character, through the same path a keystroke takes
mcp "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"cordial_text\",\"arguments\":{\"text\":\"$TYPED\"}}}" >/dev/null
sleep 5
pidof cordial-run >/dev/null || fail "the client died while typing (step 4)"
note "step 4 ok: client survived typing"

# 5. photograph the composited result. Not cordial_screenshot: it reads the
#    engine's swapchain and cannot see a GTK editor.
shoot "$SHOT"
[ -s "$SHOT" ] || { reap; fail "no composited screenshot (step 5)"; }
note "step 5 ok: $SHOT ($(stat -c%s "$SHOT") bytes)"

reap
sup=$(grep -c 'pass_key_event suppressed' "$LOG")
# Not `nativePassKeyEvent`: that line comes from `super::trace`, which is
# CORDIAL_TRACE=1, and that aborts the engine -- so it never appears and counting
# it measures nothing. The trace_text line below is the one that actually fires.
game=$(grep -cE 'pass_key_event down=' "$LOG")
note "keys: $sup suppressed, $game reached the game's key handler"
[ "${CORDIAL_KEYS_TO_GAME_WHILE_TYPING:-}" = "" ] && [ "$game" -gt 0 ] &&
  note "        WARNING: characters reached the game while a box had focus"
echo "PASS -- look at $SHOT to judge placement; the steps above prove the path."
