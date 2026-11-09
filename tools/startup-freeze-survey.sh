#!/usr/bin/env bash
# One launch of the client, with no input at all, measured. Prints one TSV row.
#
# This exists because the startup freeze is a race and every conclusion drawn
# about it from a handful of launches has been wrong. Measured on 2026-08-25
# over twenty-five launches of one signed-in profile: **eight frozen, seventeen
# healthy**. At a 32% base rate, three runs agreeing happens about one time in
# thirty, and this bug has twice been "reliably reproduced" and twice been
# something else. Do not believe a change to it on fewer than about twenty runs
# in each arm.
#
#   tools/startup-freeze-survey.sh <run-number> [profile]
#
# Columns:
#
# **2026-08-26, and this is the finding to start from: the freeze is a
# signed-in phenomenon.** Twenty runs on a signed-in profile froze SIXTEEN
# times. Forty runs earlier the same evening on a signed-out one froze not
# once. Same machine, same binary, same hour.
#
#     signed in  (ready=RootSwitchNavigator)   16 / 20 frozen   80%
#     signed out (ready=Landing)                0 / 40 frozen    0%
#
# A frozen signed-in run stops almost immediately -- presents of 1, 2, 4 and 7
# at ten seconds and unchanged at twenty-five -- against about 320 for a
# healthy one. There is no ambiguity in the reading and no need for statistics
# to separate the arms.
#
# **So every measurement taken on a signed-out profile is about something
# else**, including the twenty-per-arm nudge experiment recorded below, which
# concluded nothing because nothing froze. That was not bad luck. Sign in
# before measuring this bug.
#
# It also turns an intermittent bug into a reproducible one. At 80% a frozen
# client is two launches away, which makes catching one under gdb -- with
# `cordial_loopers` for the descriptor counts and the process CPU beside the
# stacks -- a matter of minutes rather than a day of waiting.
#
# **2026-08-26 (earlier): forty runs, twenty per arm, and not one freeze.** Nudged versus
# not, strictly interleaved, on this machine after a reboot. At the 32% base
# rate above you would expect about thirteen; the chance of none is about three
# in ten million. So the freeze did not reproduce at all that day, and the arm
# that was supposed to test it could not be judged -- you cannot measure whether
# input prevents a freeze when nothing freezes.
#
# **The compositor is no longer fixed to sway.** `COMPOSITOR=kwin` uses KWin's
# virtual backend, `HEADLESS_DIRECT=1` runs it on this host instead of in the
# distrobox, and the default is the first backend able to launch. `NOFOCUS=1`
# keeps its meaning on both: sway with a `no_focus` rule, KWin with a focus
# holder and focus-stealing prevention. See
# `docs/analysis/headless-compositors.md`.
#
# The nudge instrument was live this time, which is the part worth keeping. The
# previous NUDGE arm was a silent no-op: the virtual pointer holder was started
# after the client had already latched seat capabilities at open(), so nothing
# was ever delivered and the arm measured nothing at all. This one drove
# Cordial's own entry points through the MCP instead and recorded proof --
# `first_ok_move_at=0.017 ok=1710 errreply=0` per run. It also had a visible
# effect on something: nudged runs sit at a p25 median of 3578 presents against
# 2838, which is the idle throttle being held off, exactly as it should be.
#
# What that leaves is an open question rather than an answer. Either the freeze
# has been fixed by something committed since 2026-08-25, or its conditions
# differ from this harness. Note that every run here reached `Landing` -- the
# signed-out screen -- while the original 25-run measurement was taken on a
# signed-in profile. **Before spending another day on the freeze, re-run this
# on a signed-in profile that reaches a game**, because a bug that only appears
# past the login screen would look exactly like this.
#
#   run         the number you passed, so rows collate
#   ready       the last `app ready:` screen the shell reached
#   presents10  vkQueuePresentKHR count ten seconds after that
#   presents25  the same fifteen seconds later
#   cookies     how many domains the saved session restored
#   verdict     FROZEN when the count stopped climbing and stayed under ten
#   stage       the last `StartupController started: stage` the ENGINE logged
#
# **No input is driven, deliberately.** The freeze is reported as happening when
# nobody touches the client while it loads, so touching it measures something
# else -- and a run that was nudged is not comparable with one that was not.
#
# `presents=1` is the usual frozen reading but not the only one: the survey saw
# 0, 1 and 5. Read the count, not the verdict, when a row looks odd.
#
# `GAME_ACTIVITY=0` drops `--game-activity`, which is the arm worth running.
# **Two working references say that path is the wrong one.** Roblox's own
# Android build does not take it -- `EnableGameActivity8` resolves to false in
# `docs/traces/waydroid-roblox-startup.log.gz:626`, and no GameActivity native
# appears anywhere in that capture -- and mocktail defaults all three of its
# GameActivity switches off with the comment that the path "currently stalls V2
# init on surface flags" and that its lifecycle callbacks "can block on
# `android_app_set_activity_state` in some Linux shims"
# (`src/legacy/legacy_runtime.cc:2867-2873`, Apache-2.0).
#
# Cordial drives the ASMA/V2 app-bridge *and* GameActivity on top of it, which
# is the combination that comment names. Whether that is this freeze is a
# measurement and not an argument, and this is the switch that takes it. The
# default is on, so every row measured before 2026-08-27 stays comparable.
#
# Set `CORDIAL_POLL_COALESCE_US=0` to run the looper's idle backoff off as a
# control, `CORDIAL_BRIDGE_DELAY_MS=<n>` to hold the app bridge back before it
# starts the Lua app, and `TAG=<word>` to keep one arm's logs from overwriting
# another's.
#
# `LOAD=<n>` runs n busy loops for the duration of the launch. **Suspect the
# machine before the code here:** eight consecutive launches on an otherwise
# idle machine came back healthy, against a third frozen measured earlier the
# same evening while builds and greps were running alongside. That is not proof
# of anything yet, but it means the first two arms of this survey compared code
# while the load differed, and neither of them is safe to quote.
#
# `NUDGE=1` drives real pointer motion through the nested compositor from the
# moment the client starts until the shell is ready. That is the one arm that
# is *not* a clean measurement of the bug -- it is a measurement of the user's
# own report, which is that giving the client input while it loads stops it
# freezing. It has never been tested. It is not a fix and must not become one:
# see AGENTS.md on synthesising input, and note this only ever happens inside
# the harness's own compositor.
set -uo pipefail
RUN="${1:-0}"
PROFILE="${2:-default}"
# On unless explicitly turned off, so this switch cannot silently change what
# every previous row in this file measured.
GAME_ACTIVITY="${GAME_ACTIVITY:-1}"
[ "$GAME_ACTIVITY" = "0" ] && GAME_ACTIVITY=""
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${CORDIAL_FREEZE_OUT:-/tmp/cordial-freeze}"
mkdir -p "$OUT"
APK="${CORDIAL_APK:-$HOME/.var/app/org.vinegarhq.Sober/data/sober/packages/x86_64/com.roblox.client/base.apk}"
LIB="${CORDIAL_LIB_DIR:-$HOME/.cache/cordial/lib/x86_64}"
LOG="$OUT/${TAG:-run}$RUN.log"
SOCK="$HOME/.local/share/cordial/profiles/$PROFILE/devctl.sock"
COMP="$OUT/compositor-${TAG:-run}$RUN"
STATE="$COMP/state.json"

# Never `pkill -f`: the pattern matches the shell running it, and that has
# killed the session issuing it five times in one day. `pidof` matches the
# executable, which the engine's rename of its main thread does not touch.
# **Only this profile's clients, never every client on the machine.** This
# used to kill every `cordial-run` there was, which is fine on an idle machine
# and destroys somebody else's session on a shared one -- a developer playing
# on another profile, or a live specimen of an intermittent bug. The profile
# lock already stops two clients sharing a profile, so a stale one of ours is
# all there can be to clear. `/proc/<pid>/cmdline` is NUL-separated, hence tr.
for p in $(pidof cordial-run 2>/dev/null); do
  if tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null | grep -q -- "--profile $PROFILE"; then
    kill -9 "$p" 2>/dev/null
  fi
done
python3 "$ROOT/tools/headless_compositor.py" stop --state "$STATE" >/dev/null 2>&1 || true
sleep 3

# Windowed every time. Cordial remembers window state per profile, and a run
# that inherited fullscreen from the previous one is a different experiment --
# that cost two runs before anyone noticed the file.
printf '{\n  "fullscreen": false,\n  "maximised": false,\n  "width": null,\n  "height": null\n}\n' \
  > "$HOME/.local/share/cordial/profiles/$PROFILE/game-window.json"

# The compositor is chosen by the harness, not baked into this script. It is
# still headless — `--virtual` under KWin, `WLR_BACKENDS=headless` under sway,
# or the backend's own headless flag — and its socket is under a private
# `XDG_RUNTIME_DIR` under `$OUT`, so nothing appears on the developer's session.
#
# `NOFOCUS=1` keeps the client's window from ever taking keyboard focus, which
# is the user's own report: press Launch, do not click the new window, and it
# freezes. Sway does that with a `no_focus` rule; KWin uses a focus-holder
# window plus its focus-stealing-prevention setting. Both are implemented in
# `tools/headless_compositor.py` so the arm means the same thing on either.
COMPOSITOR_ARGS=()
[ -n "${COMPOSITOR:-}" ] && COMPOSITOR_ARGS+=(--backend "$COMPOSITOR")
[ "${HEADLESS_DIRECT:-0}" = "1" ] && COMPOSITOR_ARGS+=(--direct)
[ "${NOFOCUS:-0}" = "1" ] && COMPOSITOR_ARGS+=(--no-focus)
# The nudge arm needs the wlroots virtual pointer, so it must not be handed a
# launch-only backend. `--require` makes that a startup error instead of an arm
# that silently delivers nothing.
[ "${NUDGE:-0}" = "1" ] && COMPOSITOR_ARGS+=(--require virtual_input)
if ! python3 "$ROOT/tools/headless_compositor.py" \
       start --state "$STATE" --out-dir "$COMP" --width 1280 --height 800 \
       "${COMPOSITOR_ARGS[@]}" >/dev/null; then
  printf '%s\tNO-COMPOSITOR\t-\t-\t-\t-\t-\n' "$RUN"
  exit 1
fi
eval "$(python3 "$ROOT/tools/headless_compositor.py" env --state "$STATE")"
DISP="$WAYLAND_DISPLAY"
[ -n "$DISP" ] || { printf '%s\tNO-COMPOSITOR\t-\t-\t-\t-\t-\n' "$RUN"; exit 1; }
trap 'python3 "$ROOT/tools/headless_compositor.py" stop --state "$STATE" >/dev/null 2>&1 || true' EXIT

# GDK_BACKEND=wayland explicitly: the container exports x11, and a GTK window
# that lands on the host's X server is a window nobody in the harness can see.
# **`env`, not a bare assignment prefix.** `${VAR:+VAR=value}` looks like one
# and is not: bash decides what is an assignment before it expands, so the
# expanded word becomes the command name and the run dies with
# `CORDIAL_POLL_COALESCE_US=0: command not found` -- which this script then
# scores as a freeze, because a client that never started never presents. That
# turned a whole control arm into noise before anyone read the log.
# **The virtual pointer has to exist before the client connects.** Cordial reads
# the seat's capabilities once, when it binds the seat, and a headless seat with
# no input device reports `capabilities: 0` -- so a pointer created afterwards is
# one Cordial will never look at, and the whole nudge arm becomes a silent no-op
# that reads exactly like a null result. It was written the other way round
# first and produced one.
NUDGING=
if [ "${NUDGE:-0}" = "1" ]; then
  HOLDER="${CORDIAL_HOLDER_BIN:-/tmp/cordial-wl-holders}/wl-pointer-holder"
  [ -x "$HOLDER" ] || { echo "no $HOLDER -- run tools/build-wl-holders.sh in the container" >&2; exit 1; }
  NUDGE_FIFO=$OUT/nudge.fifo
  rm -f "$NUDGE_FIFO"; mkfifo "$NUDGE_FIFO"
  python3 "$ROOT/tools/headless_compositor.py" exec --state "$STATE" -- \
    "$HOLDER" 1280 800 < "$NUDGE_FIFO" > /dev/null 2>&1 &
  NUDGE_HOLDER=$!
  exec 8> "$NUDGE_FIFO"
  sleep 1
  NUDGING=1
fi

# `LOAD=<n>` keeps n busy loops running for the launch. The freeze is a race and
# the machine it was reported on is a slow laptop, so whether contention changes
# the rate is a real question -- and it has to be part of the harness rather
# than "whatever else the developer happened to be running", which is what
# confounded the first two arms of this survey.
# The mark the engine log's freshness is judged against; see where it is used.
STAMP_BEFORE=$OUT/.stamp-${TAG:-run}$RUN
: > "$STAMP_BEFORE"

LOAD_PIDS=
for _ in $(seq 1 "${LOAD:-0}"); do
  ( while :; do :; done ) &
  LOAD_PIDS="$LOAD_PIDS $!"
done

# GDK_BACKEND=wayland explicitly: the container exports x11, and a GTK window
# that lands on the host's X server is one nobody in this harness can see.
#
# **`env`, not a bare assignment prefix.** `${VAR:+VAR=value}` looks like one and
# is not: bash decides what is an assignment before it expands, so the expanded
# word becomes the command name and the run dies with
# `CORDIAL_POLL_COALESCE_US=0: command not found` -- which this script then
# scores as a freeze, because a client that never started never presents. That
# turned a whole control arm into noise before anyone read the log.
# Cordial's own stdout has no timestamps, and the difference between a frozen
# startup and a healthy one opens *before* the engine's first shared log line --
# so the engine's log cannot localise it and this can. Stamped through a fifo
# rather than a pipeline, so `$!` is still the client and not the last stage of
# a pipe; and stamped by the harness rather than built into the binary, because
# a harness that can add this without touching the binary cannot invalidate the
# binary under test by adding it.
STDOUT_FIFO=$OUT/stdout-${TAG:-run}$RUN.fifo
rm -f "$STDOUT_FIFO"; mkfifo "$STDOUT_FIFO"
python3 -u -c '
import sys, time
t0 = time.monotonic()
for line in sys.stdin:
    sys.stdout.write("[%8.3f] %s" % (time.monotonic() - t0, line))
' < "$STDOUT_FIFO" > "$LOG" &
STAMPER=$!

env WAYLAND_DISPLAY=$DISP GDK_BACKEND=wayland CORDIAL_DEV_CONTROL=1 \
  ${CORDIAL_POLL_COALESCE_US:+CORDIAL_POLL_COALESCE_US=$CORDIAL_POLL_COALESCE_US} \
  ${CORDIAL_BRIDGE_DELAY_MS:+CORDIAL_BRIDGE_DELAY_MS=$CORDIAL_BRIDGE_DELAY_MS} \
  ${CORDIAL_REPORT_FOCUS:+CORDIAL_REPORT_FOCUS=$CORDIAL_REPORT_FOCUS} \
  "$ROOT/target/release/cordial-run" --lib-dir "$LIB" --apk "$APK" --host-libc \
  ${GAME_ACTIVITY:+--game-activity} --run 0 --profile "$PROFILE" > "$STDOUT_FIFO" 2>&1 &
CLIENT=$!

if [ -n "$NUDGING" ]; then
  # **Through Cordial's own entry point, because the compositor route does not
  # arrive.** This arm has now been a silent no-op three times. The first two
  # started a virtual pointer after the client had latched seat capabilities;
  # this version wrote into a holder's fifo and was measured, on 2026-08-27,
  # delivering exactly nothing -- `accepted=0` on the client's own counter
  # after a full run of it.
  #
  # devctl `move` is the one path proven to arrive: twenty moves into a live
  # frozen client took `accepted` from 0 to 20. It costs a socket that only
  # exists once the client has bound it, so this waits for the socket rather
  # than assuming it and reports what it managed.
  #
  # **Read `accepted=` in the client's own `info` before believing this arm.**
  # An arm that cannot be shown to have delivered has measured nothing, and
  # three no-ops in a row is what that costs.
  ( i=0; sent=0
    while kill -0 $CLIENT 2>/dev/null && [ $i -lt 1200 ]; do
      SK=$(ls -t "$HOME/.local/share/cordial/profiles/$PROFILE"/**/devctl.sock \
             "$HOME/.local/share/cordial/profiles/$PROFILE"/devctl.sock 2>/dev/null | head -1)
      if [ -n "$SK" ]; then
        python3 -c "
import socket,sys
s=socket.socket(socket.AF_UNIX); s.settimeout(2)
try:
    s.connect(sys.argv[1]); s.sendall(b'move %s %s\n' % (sys.argv[2].encode(), sys.argv[3].encode()))
    s.recv(4096)
except Exception:
    pass
" "$SK" $((300 + i % 400)) $((300 + i % 200)) 2>/dev/null && sent=$((sent+1))
      fi
      i=$((i + 1)); sleep 0.05
    done
    echo "$sent" > "$OUT/nudge-sent-${TAG:-run}$RUN" ) &
  NUDGER=$!
fi

for _ in $(seq 1 100); do
  grep -qE "app ready: (Home|Landing)" "$LOG" && break
  kill -0 $CLIENT 2>/dev/null || break
  sleep 1
done
if [ -n "$NUDGING" ]; then
  kill "${NUDGER:-0}" 2>/dev/null
  exec 8>&-
  kill "${NUDGE_HOLDER:-0}" 2>/dev/null
fi
for lp in $LOAD_PIDS; do kill "$lp" 2>/dev/null; done

READY=$(grep -oE "app ready: [A-Za-z]+" "$LOG" | tail -1 | cut -d' ' -f3)
[ -n "$READY" ] || READY=NEVER

ask() { timeout 10 python3 -c "
import socket
s = socket.socket(socket.AF_UNIX); s.settimeout(8)
try:
    s.connect('$SOCK'); s.sendall(b'info\n'); print(s.recv(4096).decode().strip())
except Exception as e:
    print('err', e)
" 2>/dev/null | grep -oE "presents=[0-9]+" | cut -d= -f2; }

sleep 10; P10=$(ask); sleep 15; P25=$(ask)
COOKIES=$(grep -oE "\[cookies\] restored [0-9]+ domain" "$LOG" | grep -oE "[0-9]+" | tail -1)
[ -n "$COOKIES" ] || COOKIES=none

# The engine's own narration, which is the instrument that finally separated a
# frozen run from a healthy one. It prunes its own directory, so read it now.
# `profiles/`, not `instances/` -- the latter holds no `_Player_` logs at all.
# And only if it is *this* run's: a run that produced none would otherwise be
# scored from the previous run's log, silently, which is the worst kind of
# wrong number because it looks plausible.
# **Against a stamp taken before the launch, not against `$LOG`.** The stdout log
# keeps being written after the engine has stopped, so on a frozen run the
# engine log is older than it and the freshness test failed -- on exactly the
# runs whose engine log is the evidence. Six of them were scored `stage=none`
# and had their logs pruned before anyone noticed.
ELOG=$(ls -t "$HOME/.local/share/cordial/profiles/$PROFILE/data/files/appData/logs/"*_Player_*.log 2>/dev/null | head -1)
if [ -n "$ELOG" ] && [ "$ELOG" -nt "$STAMP_BEFORE" ]; then
  STAGE=$(grep -oE "StartupController started: stage = [0-9]+" "$ELOG" | tail -1 | grep -oE "[0-9]+$")
  cp "$ELOG" "$OUT/${TAG:-run}$RUN.engine.log" 2>/dev/null
else
  ELOG=
fi
[ -n "${STAGE:-}" ] || STAGE=none

VERDICT=HEALTHY
[ -z "$P10" ] && P10=0
[ -z "$P25" ] && P25=0
[ "$P10" = "$P25" ] && [ "$P25" -lt 10 ] && VERDICT=FROZEN

# `CAPTURE=1` photographs a frozen client before it is killed. Off by default
# because it costs a gdb attach on every frozen run and the survey's job is to
# count them, not to explain one -- but at an 80% rate on a signed-in profile,
# catching one alive is two launches away and this is where to do it.
if [ "${CAPTURE:-0}" = "1" ] && [ "$VERDICT" = "FROZEN" ]; then
  CAP="$OUT/capture-${TAG:-run}$RUN"
  mkdir -p "$CAP"
  # Every ALooper: owning thread, registered descriptor count, poll and event
  # counts. This is what separates stuck from merely idle -- a backtrace shows
  # epoll_wait either way, and fds=0 means nothing but a wake can ever make
  # that poll return.
  python3 -c "
import socket
s = socket.socket(socket.AF_UNIX); s.settimeout(10)
try:
    s.connect('$SOCK'); s.sendall(b'loopers\n')
    buf=b''
    while not buf.endswith(b'\n'):
        c=s.recv(65536)
        if not c: break
        buf+=c
    print(buf.decode(errors='replace').strip())
except Exception as e:
    print('err', e)
" > "$CAP/loopers.txt" 2>&1
  # **The CPU beside the stacks**, because a spinning pump and a blocked one
  # produce identical backtraces and this is the reading everyone gets wrong.
  ps -o %cpu=,rss=,stat= -p "$CLIENT" > "$CAP/cpu.txt" 2>&1
  # gdb, not lldb: on a genuinely deadlocked client lldb produced one frame per
  # thread, twice, and gdb walked the same process and named both halves.
  PATH=/home/linuxbrew/.linuxbrew/bin:$PATH timeout 240 \
    gdb -p "$CLIENT" -batch -ex 'thread apply all bt 16' > "$CAP/bt.txt" 2>&1
  # **What the descriptors actually are.** The census names them by number;
  # /proc says whether a number is an eventfd, a pipe, a socket or a file, and
  # an epoll fd's fdinfo lists the `tfd:` it is watching. Between the two there
  # is no guessing left about what the spinning looper is waiting on.
  ls -l "/proc/$CLIENT/fd" > "$CAP/fds.txt" 2>&1
  for f in /proc/$CLIENT/fdinfo/*; do
    printf '=== %s\n' "$f" >> "$CAP/fdinfo.txt"
    cat "$f" >> "$CAP/fdinfo.txt" 2>&1
  done
  # Per-thread state, so the spinning thread can be told from the idle ones
  # without a debugger: R is running, S is sleeping.
  for t in /proc/$CLIENT/task/*; do
    printf '%s %s\n' "$(basename "$t")" "$(awk '{print $3}' "$t/stat" 2>/dev/null)" \
      >> "$CAP/threads.txt"
  done
  [ -n "$ELOG" ] && cp "$ELOG" "$CAP/engine.log" 2>/dev/null
  cp "$LOG" "$CAP/stdout.log" 2>/dev/null
  # **The decisive experiment, run on the specimen while it is still alive.**
  #
  # Three captures agree on the shape: an engine thread spinning on its own
  # app-glue command pipe, and Cordial's main thread with no Wayland event for
  # twenty-five seconds. That is consistent with a starvation cycle -- the
  # engine waits for a command, the command comes from work the main thread
  # drives, the main thread waits for events, and the events come from
  # presenting, which needs the engine. If that is what this is, then one input
  # event should break it, and the user's original report is exactly that:
  # it freezes if you do not touch it.
  #
  # So: poke it through Cordial's own entry point -- never the compositor --
  # and read the present count again. A count that climbs after the poke and
  # not before is the mechanism confirmed.
  python3 -c "
import socket, time
def ask(line):
    s = socket.socket(socket.AF_UNIX); s.settimeout(8)
    try:
        s.connect('$SOCK'); s.sendall((line+'\n').encode())
        buf=b''
        while not buf.endswith(b'\n'):
            c=s.recv(65536)
            if not c: break
            buf+=c
        return buf.decode(errors='replace').strip()
    except Exception as e:
        return 'err %s' % e
print('before   ', ask('info'))
for i in range(20):
    ask('move %d %d' % (400+i*3, 300+i*2))
    time.sleep(0.05)
time.sleep(4)
print('after 20 pointer moves', ask('info'))
time.sleep(4)
print('4s later ', ask('info'))
" > "$CAP/poke.txt" 2>&1
  echo "captured a frozen client into $CAP" >&2
fi

kill -9 $CLIENT 2>/dev/null
kill "${STAMPER:-0}" 2>/dev/null
rm -f "$STDOUT_FIFO"
python3 "$ROOT/tools/headless_compositor.py" stop --state "$STATE" >/dev/null 2>&1 || true
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$RUN" "$READY" "$P10" "$P25" "$COOKIES" "$VERDICT" "$STAGE"
