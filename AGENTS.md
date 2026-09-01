# AGENTS.md

Instructions for any coding agent working in this repository. Human contributors
should read [CONTRIBUTING.md](CONTRIBUTING.md), which says the same things at
more length and explains why.

Cordial loads Roblox's official Android x86-64 `libroblox.so` natively on Linux:
a ported AOSP bionic linker, a bionic/glibc shim, libjnivm in place of Android's
ART, and a framework layer that answers the calls the client makes into the
platform.

## Reach for the MCP first

**`tools/cordial-mcp.py` is how this client is driven, inspected and debugged.
Prefer it to anything you would otherwise write yourself.** Start the client and
attach:

```bash
just dev --play      # opens the window and starts playing, no button press
just mcp             # what an agent's MCP configuration runs; finds the socket itself
```

`just dev` and `just client` turn the control surface on by default, and `just
mcp` finds whichever client bound a socket most recently. There is no pid to
look up and nothing to configure.

What it gives you, and why each one exists:

- **`cordial_screenshot`** reads the frame out of Cordial's own Vulkan
  swapchain, so it is unaffected by occlusion, by another window covering the
  client, or by the window being off-screen. It is also the only thing on this
  host that can photograph a Wayland window at all — five other routes were
  tried and every one was refused by the compositor or the kernel.
- **`cordial_info`** returns the present count. **Called twice a few seconds
  apart it is the single best test for a wedged client**, because a wedged
  engine leaves it fixed while everything else keeps running. That reading — 42
  presents against 74 million polls — is what finally characterised the
  2026-08-21 freeze, in seconds, after a day of guessing.
- **`cordial_click` / `cordial_move` / `cordial_key` / `cordial_text` /
  `cordial_scroll`** drive Cordial's own input entry points. Never synthesise
  input at the compositor; see the caution near the end of this file.
- **`cordial_loopers`** reports every `ALooper` in the process: the thread that
  owns it, how many descriptors it has registered, its poll and event counts,
  and how long since a poll last found anything. **This is what separates a
  client that is stuck from one that is merely idle** -- a backtrace shows
  `epoll_wait` either way, and `fds=0` means nothing but a wake can ever make
  that poll return. It is also how the engine's busy-poll was quantified: 9.7
  million polls a second at 99.7% of a core with the idle backoff off, 3,261 a
  second at 1.2% with it on, same frame rate.
- **`cordial_textbox`** is the only readback of typed text there is. The editor
  on a focused Roblox TextBox is a GTK widget, and `cordial_screenshot` reads
  the engine's swapchain, which cannot see one -- so before this verb existed,
  every check of typing asserted that the *path* worked and left "did the right
  characters end up in the box" to a human squinting at a screenshot. It reports
  the editor's real rectangle and which source placed it, plus the character
  count; the text itself is withheld unless
  `CORDIAL_TRACE_TEXT_SHOW_PASSWORDS` is set on the client, because a focused
  field is as often a password box as a search bar.
- **`cordial_backtrace`** attaches lldb and quotes the process CPU beside the
  stacks, because a spinning pump and a blocked one produce identical
  backtraces. **`cordial_debugger`** runs arbitrary debugger commands.

**Write a bespoke harness only when the MCP genuinely cannot answer the
question, and say why in the commit.** Every ad-hoc script written here in one
day scored something that turned out to be constant across all runs —
`mainWorkCallback`, which fires exactly twice in healthy runs too;
`onFlagsLoaded`'s byte count, which never varies; present counts taken without
input, which the idle throttle pins to 1.0/s. The MCP's tools are the ones whose
readings have been checked against a control.

It works in coordinates and pixels. There is no element tree, because Roblox
publishes no accessibility tree at all — measured four ways, see
`native/accessibility.cpp` — and getting one would mean engine introspection,
which ADR-001 and ADR-003 rule out permanently.
[ADR-019](docs/adr/ADR-019-development-control-surface.md) records the whole
design.

## The one rule

**Grep `docs/traces/` before disassembling anything.** It holds a logcat capture
of the same APK on real Android. When a question comes up about what the engine
expects, that capture is a lookup, not an investigation.

Over one long session, **nine consecutive conclusions drawn from reading the
stripped binary were wrong**, and every conclusion drawn from running something
held up. This is the single most expensive mistake available here, and agents are
unusually prone to it because reasoning from a binary feels like progress.

## Search Sober's issues before investigating a user-facing bug

`tools/sober-corpus/data/raw.jsonl` is a local copy of Sober's issue tracker,
2,000+ issues with their comments, fetched incrementally ([ADR-017](docs/adr/ADR-017-sober-issue-corpus.md)).
Sober runs the same engine on the same kind of desktop, so **almost every
user-facing symptom here has already been reported there, often years earlier
and often with the environment that distinguishes it.** It is offline, it is one
`grep`, and it costs nothing.

```bash
python3 - <<'EOF'
import json, re
pat = re.compile(r'textbox|cannot see.*typ', re.I)
for line in open('tools/sober-corpus/data/raw.jsonl'):
    d = json.loads(line)
    if pat.search(str(d.get('title',''))):
        print(d['number'], d.get('state'), d['title'])
EOF
```

**Read the comments, not just the title, and check whether it was actually
fixed.** Sober's tracker closes issues by inactivity, so `CLOSED` frequently
means "nobody replied", not "solved" -- and an issue closed unfixed is still
evidence about the symptom.

Worked example, 2026-08-24. "Characters are invisible until the box loses
focus" was being investigated here from first principles. Sober #987 reports it
verbatim -- *"when typing in chat or any other textboxes i cant see text i
typed"* -- and was closed by inactivity with a comment saying it went away after
switching to KDE Plasma. Sober #1026, still open, narrows it much further: the
textbox stops working *only in fullscreen*, only on Wayland and not X11, on
Sway/DWL/Hyprland but not KDE, and one reporter pins it to the surface being as
large as the output -- "stops when either is even a pixel below that". None of
that was going to come out of reading our own code, and it is the difference
between "the engine never draws focused text" and "the engine's drawing of
focused text is sensitive to compositor and surface size".

## Verify by running. Do not report what you have not observed

A claim about this engine is worth what it was measured with.

- **Never state a result you did not see.** Not "this should now work", not "the
  fix is complete" — run it and paste what it printed. If you cannot run it, say
  that plainly.
- If a claim cannot be tested, label it **`INFERRED`** in the comment and in the
  pull request. That is an acceptable state. Presenting it as established is not.
- Stability and timing claims need **repetition**. One clean run is not a result;
  one bug here reproduced on roughly one launch in three.
- Use a **control**. Show the behaviour changes when the thing you changed is
  turned off, in the same session.

If you find something already written down is wrong — a comment, `docs/NEXT.md`,
an ADR — **say so plainly and correct it**. Several commits exist only to retract
an earlier claim. That is the highest-value contribution here.

## Which issue this is

Route work by shape. The templates in `.github/ISSUE_TEMPLATE/` are GitHub issue
forms — required fields, not just headings a submitter can delete — and carry
the diagnostics for each.

| Shape | Template | Recognise it by |
|---|---|---|
| A new Roblox build fails to load, or needs symbols we lack | `roblox_update.yml` | `cannot locate symbol` at load, or a called stub on exit |
| An Android expectation is unanswered, so a feature silently does nothing | `broken_feature.yml` | `Constructed Unresolved symbol` in the jnivm log; audio is the live example |
| Something Cordial or a plugin should be able to do | `feature.yml` | No engine call is involved |
| Cordial misbehaves at something it already does | `bug_report.yml` | It used to work, or clearly should |
| You established or disproved something | `finding.yml` | The output is knowledge, not code |

**Missing symbols.** `docs/analysis/undefined-symbols.tsv` generates the stub
table. To find what a build needs that it lacks:

```bash
readelf --dyn-syms -W /path/to/libroblox.so \
  | awk '$7=="UND" {print $8}' | sed 's/@.*//' | sort -u > /tmp/new.txt
cut -f2 docs/analysis/undefined-symbols.tsv | sort -u > /tmp/old.txt
comm -23 /tmp/new.txt /tmp/old.txt
```

Data symbols fail the `DT_NEEDED` walk at load time rather than at first use, so
one missing name stops the whole client.

## Never make a stub lie

A stub that returns success is worse than one that returns failure. The engine
proceeds on an answer that is not true and fails somewhere with no relationship
to the cause. `native/opensles.cpp` reports
`SL_RESULT_FEATURE_UNSUPPORTED` rather than handing back a dead engine object;
that is the pattern. Reporting failure keeps the gap where someone can find it.

## Permanently out of scope

**No in-process code execution against the Roblox process.** No hooking, no
memory patching, no injected script environment, and no API by which a plugin
could request one. Not disabled — *absent*, so there is no primitive to extract
or re-enable in a fork. [ADR-001](docs/adr/ADR-001-in-process-hooking.md),
[ADR-003](docs/adr/ADR-003-plugin-isolation.md).

**No Roblox code, ever.** No APK, asset, or decompiled material committed,
vendored, or pasted into an issue. Observing a running binary is fine and is how
nearly everything here was established — call order, load order, argument shapes,
syscalls, timing, and method prototypes declared in the dex. Transcribing a
decompilation of *how it implements* something is not. The line is not the tool,
it is what you take away. Any `decompiled/` directory is off-limits.

**No client-side integrity flags, watermarks, or obfuscation-as-security.**

Asset overlays **are** in scope, non-destructively and off by default
([ADR-010](docs/adr/ADR-010-plugin-asset-overlays.md), superseding ADR-004).

## Plugin capabilities expose effects, not channels

A plugin never receives a socket, a D-Bus connection, or a file descriptor.
Cordial holds the permission and performs the effect; the plugin sends a payload.
`presence.set` takes a presence structure and Cordial owns the Discord socket.

A broker should be a payload type and an effect. If a proposed capability needs a
design document, it is too broad and wants splitting.
[ADR-007](docs/adr/ADR-007-host-resources-are-brokered.md).

## The ADRs are the decision record

`docs/adr/` records what was decided and why, including reversals. Before
proposing something that contradicts one, read it.

**Arguing with an ADR is welcome and expected** — ADR-004 was reversed exactly
that way, by someone pointing out the reasoning did not hold. What is not
acceptable is quietly contradicting one in code. If a change makes an ADR wrong,
update the ADR in the same change, and mark the old one superseded rather than
deleting its reasoning.

## Style

Read the surrounding file before writing. This codebase has a consistent voice
and matching it is not optional.

- **Comments explain *why*, anchored in the failure that motivated the code.**
  Not what the line does. The good ones name the bug that would otherwise recur.
- British-ish prose. No emoji in code or comments. No bullet-list comment blocks.
- **Commit messages say what you measured**, not only what you changed. They are
  long here on purpose.
- **Do not paste the conversation into them.** A commit that quotes what
  somebody typed in chat -- "reported as \"its stuck on starting up\"" -- turns
  a private exchange into a permanent public record, and it reads as though the
  transcript were the evidence. It is not: the evidence is the measurement.
  Write the *fact*: which symptom, on what, and what the run showed. "The status
  never advanced past the launch event, because `client.ready` does not always
  arrive" says everything the quote did and stands on its own to somebody
  reading the log in a year. Same for screenshots, chat handles and anything
  else that only exists because of how the work happened to be requested.
- Prefer correcting a stale comment over leaving it. A comment that lies costs
  more than no comment.

## Documentation is part of the change, not after it

**A user who has to ask in Discord is a documentation bug, and it is the kind
this project keeps shipping.** On 2026-08-28 two people asked, an hour apart,
where to find plugins and how to install one. Cordial had shipped a plugin
system, an installer, a signature-checked registry format, a permission model
and three working plugins -- and the README had no plugins section at all. The
only writing on the subject was `plugins/README.md`, which is addressed to
somebody authoring one.

The same day, one of them guessed the archive was a `.tar.gz`. It is a
`.tar.zst`, and nothing anywhere said so. They would have got a confusing
failure from a reasonable guess.

So:

- **If a change makes a document wrong, fix it in the same change.** Not in a
  follow-up, which is where documentation goes to not happen. The commit that
  renamed the first-run button to "Download Roblox" left the README offering
  "Download it for me" -- a control the user could not find, which is worse than
  saying nothing.
- **Document what exists, never what is planned.** A README describing an
  unshipped feature costs a reader an evening and costs the project their
  opinion of it. Where something genuinely does not exist, say so plainly:
  "there is a registry format and no populated registry to point you at" is a
  useful sentence and an honest one.
- **Check the claim against the thing, not against your memory of it.** A guide
  written here told users to `mkdir` and `tar --zstd -xf` when Settings has a
  file picker that does it -- teaching the hard path as the real one. One look
  at the settings window would have caught it.
- **The same rule as comments, one level up.** `pipewire_backend.h` said the
  audio backend was "announced at startup" when nothing announced anything, and
  a user spent an evening unable to tell whether their setting had been read.
  `android_classes.cpp` said the class had fourteen fields when the dex declares
  fifteen. `NOTICE` promised a list of third-party software and gave one entry
  of four, two of which are statically linked into every binary shipped.
- **A commit message is documentation and inherits the rule about unobserved
  results.** `35f38f9` listed "`CORDIAL_AUDIO_HOST=oss` selects it" among its
  verified claims. It was true of the selector, measured by calling the selector
  directly, and false of the client, where three gates meant the selector never
  ran. That is the broken instrument this file opens with, in prose.

Where the user-facing writing lives, and who it is for:

| | For |
|---|---|
| `README.md` | Somebody deciding whether to install it, and then installing it |
| `docs/plugin-api.md`, `plugins/README.md` | Somebody writing a plugin |
| `docs/adr/` | Somebody about to contradict a decision |
| `docs/analysis/`, `docs/NEXT.md` | Somebody continuing an investigation |
| Release notes in `docs/releases/` | Somebody who just installed it and hit something |

**Release notes say what is broken.** Every set here does, about a third of the
way down, because somebody installing a client that freezes on a signed-in
profile deserves to know before they meet it rather than after. A release note
that only lists what was added is an advertisement.

## Build and test

```bash
cargo build --release      # Clang required; AOSP bionic does not build with GCC
cargo test --workspace
```

Both must pass. Run them; do not assume.

**Two builds must never share one `target/`, and this is not a tidiness rule.**
On 2026-08-24 an agent working in a `git worktree` pointed `CARGO_TARGET_DIR` at
the main checkout's `target/` to save disk while the main session was building
the same crates. What came out was two `cordial-linker-sys` rlibs with different
hashes, **neither containing a symbol that was plainly in the source**, and a
link that failed on an undefined reference to it. From the other side of the
same collision the main session saw `android_classes.cpp.o` timestamped six
minutes older than `android_classes.cpp` and concluded that `build.rs` had
stopped watching `native/`, which it had not.

That failure mode is the worst one available here, because it looks exactly like
code that compiled and had no effect -- the same sentence `build.rs` uses to
explain why it watches the whole native tree. Give a worktree its own
`CARGO_TARGET_DIR` on disk and delete it afterwards. If a symbol you can see in
the source is undefined at link time, check the `.o` timestamp under
`target/*/build/cordial-linker-sys-*/out/build` and check whether anything else
was building, before believing anything about the build script.

Running the client needs an APK the user supplies — Cordial ships none. On this
machine it is at
`~/.var/app/org.vinegarhq.Sober/data/sober/packages/x86_64/com.roblox.client/base.apk`,
downloaded by Sober, with the extracted library at `~/.cache/cordial/lib/x86_64`.
**Three agents in succession have concluded no APK exists here and lost a session
each to it.** Look there before searching:

```bash
cargo run --release --bin cordial-run -- \
  --lib-dir /path/to/lib/x86_64 --apk /path/to/base.apk \
  --host-libc --game-activity --run 30
```

Useful switches: `CORDIAL_TRACE_TEXT=1` for text entry, `CORDIAL_TRACE_PATHS=1`
for path-taking libc calls, `--dump-classes <file>` for the Java surface Roblox
asked for. **`CORDIAL_TRACE=1` aborts the engine** — it wraps variadic functions
ABI-unsafely. Do not reach for it.

## Do not use present counts as a frame rate

`vkQueuePresentKHR` counts over a wall-clock window were this project's fps
metric, and they measure the wrong thing. **Presents run at about 60 a second
for thirteen seconds and then drop to exactly 1.0 a second**, identically on X11
and Wayland — an idle throttle, not a frame rate. Synthetic pointer motion holds
50–60 for a whole 240-second run and toggling it flips the rate both ways. Every
count recorded here before 2026-08-02 is that curve integrated, and several were
quoted as evidence.

If you need a frame rate, **drive input for the whole measurement** and report
the rate with the input rate beside it. With input flowing the number is a hard
FIFO vsync lock to the output's refresh — 60.0 Hz gives 60, a 50 Hz monitor
gives 49.4 even in fullscreen at four times the pixels.

**Do not measure timing under `WAYLAND_DEBUG=1`.** It changes what it measures.
Three findings taken that way — a 12.6 s pump stall, presents at 0–3/s with
input flowing, and 20–25 fps in fullscreen — all vanished on an untraced repeat
minutes later.

## Say which build you are talking about

The window title is `Cordial <version> (<commit>)` -- the version from
`Cargo.toml` and the commit from `git rev-parse --short=9`, stamped at compile
time by `crates/cordial-shell/build.rs`. A release reads `Cordial 0.11.0
(0fdbb4425)`; a build from a source drop with no git reads `Cordial 0.11.0`.

**This used to be `git describe --tags --always --dirty`, and that was wrong.**
The version and the commit are two facts, not two spellings of one: a tree whose
manifest said 0.11.0 displayed `0.10.0-26-g571e69b-dirty`, the *previous*
release, while the same binary told a mirror it was `Cordial/0.11.0` in the
User-Agent `cordial-update` builds from `CARGO_PKG_VERSION`. Two numbers that
can disagree eventually do. `Cargo.toml` is the version, it compares as semver,
and it survives a tarball, an AUR source package and the Flatpak's `type: dir`
source -- none of which has a usable `.git`. See
`crates/cordial-shell/src/version.rs`; a CI gate refuses a tag that disagrees
with the manifest.

**`-dirty` on the commit means the binary was built from a tree with
uncommitted changes.** It rides on the commit now rather than the version, which
is where it belonged. Quote the full string in any report -- or better, the
whole block from `cordial --diagnostics`, which carries it along with the
distribution and how Cordial was installed. A build made from a working tree several
agents were editing is otherwise indistinguishable from a committed one, which
cost an afternoon of chasing an input regression nobody could attribute to a
tree.

## Ask a debugger before you theorise

**lldb attaches to a running Cordial, and on a frozen one it is the fastest
answer in this repository.** A stuck client took most of a session of guessing
on 2026-08-21 -- four theories, three of them scored with instruments that were
constant across every run -- and one `thread apply all bt` settled more in
thirty seconds than any of it.

```bash
export PATH=/home/linuxbrew/.linuxbrew/bin:$PATH
lldb -p $(pgrep -f 'cordial-run.*--profile <yours>') -b \
     -o 'thread backtrace all -c 12'
```

lldb rather than gdb because Cordial is a Clang project by necessity -- AOSP's
bionic does not build with GCC -- so it is the toolchain already required.
**Not for speed:** attach-and-backtrace measured 1.63 s and 1.56 s against gdb's
1.64 s and 1.65 s here, which is a tie. gdb still works and the equivalent is
`gdb -p PID -batch -ex 'thread apply all bt 12'`.

**Keep gdb to hand, because lldb cannot unwind every stack.** This paragraph
used to say both resolved Cordial frames to file and line, which is true of an
ordinary address and false of the one that mattered. On 2026-08-22 a genuinely
deadlocked client gave lldb nothing but

    frame #0: libc.so.6`__syscall_cancel_arch_end

for every thread, twice, including with disassembly display off. gdb walked the
same process and named both halves of the deadlock. Without it the bug would
not have been found.

The cause is specific and worth knowing rather than generalising from: the
return address landed on `__syscall_cancel_arch_end+0`, a bare zero-size label
marking the end of glibc's cancellable-syscall region. At offset zero of a
symbol with no size there are no function bounds and so no unwind plan, and the
walk stops; gdb gets past it on its own heuristics. **lldb is not the weaker
unwinder in general** -- on a `sleep(300)` stopped mid-function it produced five
correct frames to gdb's four. Nor is it a missing-symbol problem: gdb reported
debuginfod off, this host has no glibc debuginfo, and it still unwound from
`.eh_frame` alone.

`cordial_backtrace` now notices a stack that did not unwind and retries with
gdb, saying so. **A one-frame backtrace is not an answer**, and reporting one as
though it were is the broken instrument this file's opening rule is about.

Easier still, and preferred, is to let the development MCP do it:
`tools/cordial-mcp.py` exposes `cordial_backtrace` (which quotes the process's
CPU beside the stack, because that is the reading everyone gets wrong) and
`cordial_debugger` for arbitrary commands. It also screenshots the client out of
its own swapchain and drives its input, which is how a freeze should be
investigated now -- see [ADR-019](docs/adr/ADR-019-development-control-surface.md).

Installation is the part worth writing down, because two other routes look
obvious and both fail. This host is immutable ostree, so `dnf install gdb` needs
`rpm-ostree` and a reboot. Containerised gdb is worse: it installs fine and then
cannot attach at all -- `ptrace: Operation not permitted` even with `--pid=host
--privileged --cap-add=SYS_PTRACE --security-opt seccomp=unconfined`, because
rootless podman puts the tracer in a user namespace that is not an ancestor of
the tracee's, and no flag fixes that. `yama/ptrace_scope` is already 0 here, so
that is not the obstacle either. **Homebrew is, and it needs neither root nor a
reboot:** `brew install gdb` drops a working gdb 17.2 in `$HOME`. `eu-stack`
comes from the same place and is a decent second best -- it needs no symbols and
is a single command -- but it leaves libroblox frames as bare addresses and, more
importantly, printed the async-io reactor's lock as an unresolved frame where gdb
named `Mutex<polling::Events>` and the source line.

What to read first, and what each answer means:

- **The main thread.** Healthy, it sits in `epoll_wait` inside
  `looper::pump` and spins -- about 2.5 M polls a second with the census on. The
  same stack at 0.4% CPU means it is *blocking* rather than polling, so the
  engine asked for a blocking wait and nothing woke it. That distinction is
  invisible in a backtrace alone: **always quote the process's CPU beside the
  stack**, or the two states look identical.
- **A thread in `Mutex::lock_contended`** is worth a second look and usually
  innocent. `async_io::driver::main_loop` waiting on `Mutex<polling::Events>`
  while a `zbus::Connection` thread sits in `polling::Poller::wait` on the same
  `REACTOR` singleton is async-io working exactly as designed -- any thread may
  become the poller and the driver waits its turn. Check the addresses match the
  same reactor before calling it a deadlock.
- **Zero CPU does not disprove a deadlock.** A thread blocked on a futex burns
  nothing; that is what blocking is. This was got backwards once already, and it
  killed a live lead for an afternoon.

Sample twice. A single `bt` catches transient states, and a lock that is still
held sixty seconds later is a different claim from one seen once.

## Two practical cautions

**Never synthesise input with `XTestFake*`, `ydotool`, `wlr-virtual-keyboard`,
the `RemoteDesktop` portal, or anything else that injects at the compositor.**
It lands on whatever has focus, which is the developer's session. This has
already hijacked a developer's cursor once mid-session.

This rule used to end "window-targeted `XSendEvent` only", which no longer means
anything — [ADR-011](docs/adr/ADR-011-wayland-and-libadwaita.md) is Wayland, and
Wayland has no window-targeted injection. To drive Cordial's own input, call
`input::pass_key_event`/`input::pass_text` directly; Cordial is the client, so
there is nothing to send through. To drive somebody else's window, nest a
headless compositor on its own `WAYLAND_DISPLAY` and inject inside that.

**To type real keys at Cordial's own widgets, hold the device open.**
`tools/build-wl-holders.sh` builds two small clients that create a virtual
keyboard and a virtual pointer inside a nested compositor and *keep* them:
`wlrctl` and `swaymsg seat - cursor` both create the device and exit in the same
breath, and the compositor has not acted on it yet. It matters for more than
reliability -- a headless seat with no device reports `capabilities: 0`, and
Cordial reads seat capabilities once at `open()`, so under `cage` it never binds
its own `wl_keyboard` at all and the guard that stops GDK and Cordial both
inserting a character never runs. `tools/text-input-e2e.py` is the worked
example; it asserts on `cordial_textbox` and fails rather than warns.

**Do not test with an account anyone cares about**, and keep test accounts on a
separate IP. The risk is collateral rather than causal: enforcement is automated,
runs in waves, and associates accounts sharing an address.

**Give your runs their own data root.** A profile is held by one instance at a
time, by `flock`, so a second launch against it is refused rather than allowed
to corrupt Roblox's storage ([ADR-012](docs/adr/ADR-012-profiles-and-instances.md)).

**That became true of `cordial-run` on 2026-08-22, and was not before.** This
file said it for three weeks while only shell-launched clients took a claim; a
hand-run `cordial-run --profile X` took none, and four `--profile CordialTest`
engines were measured running at once with nothing refused. If you are reading a
report or a comment written before that date which relies on the lock, it is
wrong. `cordial-run` now claims the profile before it touches anything in it,
and hands it back by exiting, however it exits.

**So expect to start being refused where you were not.** A second client on the
same profile now stops with `profile "default" is already open in another
Cordial client`, names the process holding it, and exits 3. That is the lock
working, not a regression — but everyone here has been running several clients
against `default` without knowing it was unprotected, so the habit breaks
before the message is read.

Everything defaults to the `default` profile, so several agents testing at once
collide on that lock and read the refusal as a bug in whatever they were
working on:

```bash
XDG_DATA_HOME=~/.cache/cordial-agent-<yours> just client --run 30
```

Both the shell's profile root and the client's data directory derive from
`XDG_DATA_HOME`, so that redirects the lot. Use a path on disk — `/tmp` is tmpfs
and comes out of RAM — and delete it when you are done. `CORDIAL_PROFILE_ROOT`
redirects `profile.rs` only, which is what the unit tests use; it does **not**
move the client, which still hardcodes its own path.
