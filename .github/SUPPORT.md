# Support

## Report a bug, ask for a feature, or write down a finding

[Open an issue](https://github.com/luohoa97/cordial/issues/new/choose) and
pick the template that matches. Every one of them asks for the same
**Diagnostics** block — get it from **Report a Problem** in
Cordial's main menu, or run `cordial --diagnostics` (`cordial-shell --diagnostics` from a
checkout, or `flatpak run io.github.luohoa97.Cordial --diagnostics` under
Flatpak). It carries the Cordial and Roblox build, your kernel and
distribution, and how Cordial was installed — no account, no token, no
profile name, and nothing from your home directory. It is shown on screen
before it is copied, so read it before you paste it.

The bug-shaped templates also ask for a **log archive**: **Report a Problem →
Save logs…** writes one `.zip` with the diagnostics, the launcher's and the
client's output, the engine's newest logs and whether a core dump exists, and
drag it into the issue. Cookies, tickets, link queries, user ids, usernames and
typed text are removed first; it is pattern matching, so look inside if the
account matters to you. No core file is ever in it.

Five templates, routed by shape:

| Template | Use it when |
|---|---|
| **Bug report** | Something behaves differently from what you expected |
| **A Roblox feature does not work** | Something Android would answer that Cordial silently does nothing for |
| **Roblox updated and something broke** | A new Roblox build fails to load, or reaches for a symbol Cordial does not have |
| **Feature or capability** | Something Cordial, or a plugin, should be able to do |
| **Finding** | Something you established about the engine, including something that turned out to be wrong |

There is no blank-issue option — a report without the shape one of these
gives it is much harder to act on, and the templates exist so nobody has to
guess what to include.

## Get help, or just talk to us

For getting Cordial running, quick questions, and what is currently being
worked on, [the Discord](https://discord.gg/qJzU3Xfr9b) is faster than an
issue and does not need a template. Bugs and feature requests found there
still belong in an issue afterwards — chat is not searchable and does not
get triaged, so something reported only there is easy to lose.

## Security issue

Report it privately through [a GitHub security
advisory](https://github.com/luohoa97/cordial/security/advisories/new)
rather than in a public issue.

## Before you file anything

[`docs/NEXT.md`](../docs/NEXT.md) says what already works, what is blocking,
and what has already been ruled out — including a fair number of things that
looked like bugs and were not. It is worth a look before writing a report
from scratch.
