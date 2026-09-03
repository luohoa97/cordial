# ADR-030: Reports arrive from Discord, as forms rather than as messages

**Status:** Proposed
**Date:** 2026-09-03
**Related:** [ADR-017](ADR-017-sober-issue-corpus.md)

## Context

People report bugs in Discord and not on the tracker, and the reason is not
laziness: filing an issue needs a GitHub account, and a user who has one still
has to leave the place they were already talking in. So the reports arrive as
chat, get answered once, and are gone — which is the same loss `AGENTS.md`
already describes from the other direction, where a symptom already reported in
Sober's tracker gets investigated here from first principles because nobody
searched.

The obvious existing answer is [`belst/discordissues`](https://github.com/belst/discordissues),
which was suggested for exactly this. **It does not fit, and the reason is
specific rather than a matter of taste.**

It creates an issue when somebody reacts 🐛 to a Discord message, and the
message body becomes the issue body. `.github/ISSUE_TEMPLATE/config.yml` sets
`blank_issues_enabled: false` on purpose, and says why: every issue goes through
one of the five forms because that is where the required Diagnostics block
lives, and "this project has already learned what a report without a build
number or an install method costs". A reaction bridge produces precisely the
issue that setting exists to refuse — with the added cost that it now looks like
a filed report rather than an unanswered question.

Three practical points besides. It was last pushed on 2022-04-01 and has no
stars. It carries **no licence at all**, so it may be read and never copied
from — the rule `CLAUDE.md` states for Nuah, for the same reason. And it wants
a gateway bot, a GitHub App, a database and a public webhook endpoint, which is
a service to keep alive.

What it gets right is the half worth taking as an idea: it opens a **thread**
per issue and syncs it both ways, thread messages becoming issue comments and
`issue_comment` webhooks becoming thread messages. That is what makes a report
from somebody without a GitHub account answerable, and it is the part of the
design that matters.

## Decision

**A Discord user files a form, not a message, and the forms are generated from
`.github/ISSUE_TEMPLATE/` rather than written twice.**

A pinned message offers one button per template. A button opens a Discord modal
carrying that template's fields; submitting it creates the GitHub issue with
the same labels and the same field headings the web form produces, opens a
thread, and links the two.

`tools/discord-bridge/` is the implementation: Deno TypeScript, matching
`tools/sober-corpus/` beside it and the runtime Cordial already ships. Its
`issue_forms.ts` is a check as much as a converter. Discord allows a modal **1 to 5 top-level components**, verified
against the component reference on 2026-09-03 rather than remembered, so the
templates do not all fit as they stand. What it does about that is the decision:

* **A required field that will not fit is an error and exits non-zero.** Filing
  an issue without Diagnostics is the failure this whole ADR is about.
* **Optional fields that do not fit are dropped and named**, never silently.
  Today that is four fields on `bug_report` and one on `roblox_update`; the
  other three templates fit whole.
* A modal `Label` description is one short line where a form's is a paragraph,
  so ten fields carry a Discord-length phrasing chosen for the space. **The
  fields are still derived**; only that phrasing is written by hand, and a
  phrasing naming a field no template has any more is an error.

Dropdowns survive, which is what makes this possible at all: `String Select` is
allowed inside a modal, so `broken_feature`'s two dropdowns and `finding`'s one
are asked exactly as the web form asks them.

**No database.** Discord will POST interactions to an HTTP endpoint rather than
requiring a gateway connection, so the bridge is a stateless function. The
issue-to-thread pairing is stored in the two artefacts themselves — the thread
id in a hidden comment in the issue body, the issue number in the thread's
opening message — so there is no state to lose, migrate or back up, and a
thread that gets deleted takes only itself.

**The issue is filed by the bridge, and says who reported it.** A user without
a GitHub account cannot be the author, so the body names the Discord reporter
and links the thread. Follow-up questions go to the thread, which is the point.

## Consequences

The tracker keeps its guarantee: every issue still arrives through a form with
Diagnostics in it, whichever side it came from. `blank_issues_enabled: false`
is untouched.

Two forms lose optional fields on the Discord path. A user who has that detail
can add it in the thread, where it becomes a comment on the issue — so the
information is not lost, only asked for later.

Somebody must hold a Discord bot token and a GitHub App key, and a public
endpoint must exist. That is real operational surface and it is the strongest
argument against doing this at all; it is accepted because the alternative is
reports that never reach the tracker.

A generated form can drift from a template only by the generator failing, which
is the intended direction. Running `deno task check` in CI is what makes
that true rather than aspirational.

**This ADR is proposed, not accepted.** The bridge is written and its 42 tests
pass -- signature verification against an independently generated key, the App
key against `openssl` as an oracle, the whole interaction flow against fakes,
and the stale-template fallback -- but **nothing has been run against a real
Discord server or a real GitHub App.** The component rules are read from
Discord's reference and the modals are generated and inspected, which is not
the same as watching somebody file an issue.
