# ADR-030: Reports arrive from Discord, as forms rather than as messages

**Status:** Accepted
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

## Status: accepted, 2026-09-04

It is running. `cordial-issues` is a Cloudflare Worker deployed from `main` on
every push, filing into `luohoa97/cordial` and posting into `#issues`, and a
real bug report has been filed through it by a person who did not use GitHub to
do it.

Two decisions in the draft above turned out to be load-bearing, and one gap had
to be filled.

**The thread is the issue's other face, not a notification.** The draft had
comments travelling both ways and stopped there. In practice the reporter also
needs to be able to *finish* the thing they started -- they cannot close it on
GitHub, having no account there, so filing without closing was half a
permission. The thread now carries comment, close, reopen and mark-completed,
and a close or reopen on GitHub moves the thread to match.

**Close and complete are two different claims**, and separating them is the
part most worth keeping. "I do not need this any more" is the reporter's to say
and closes as `not_planned`; "this is fixed" is a claim about the project and
closes as `completed`. They are gated differently -- the first against the
reporter recorded in the issue, the second against Discord permissions -- and a
tracker that let the two look alike would stop being able to answer what was
actually fixed.

**Who may act is read from the issue, never from the button.** A `custom_id` is
client-supplied and anybody who can see a message can press it. The reporter's
id therefore lives in the issue body, which only the App can write, and is
re-read on every press.

**Deno Deploy was tried first and abandoned.** Builds and deploys went green
while every revision stayed "Revision inactive" with no domain attached, and
verifying the organization changed nothing; the app was demonstrably healthy
locally throughout. Cloudflare Workers took the same handler after a small port
-- configuration passed in rather than read from a global, and npm `yaml` in
place of a `jsr:` specifier its bundler cannot resolve.

Three bugs are worth recording because each was invisible from the side it
broke on:

* a PEM in an environment variable arrives with escaped newlines, and the
  whitespace strip left an `n` inside the base64. The isolate died before it
  could log, so a green deploy served nothing;
* a promise left running when a Worker's fetch handler returns is **cancelled**
  -- correct under Deno, fatal here -- so the first real report sat on "is
  thinking" for ever and no issue was created. `ctx.waitUntil` is the contract;
* `IS_COMPONENTS_V2` alongside `content` is refused outright, so an issue was
  filed and its thread was not.

The last of those was fixed by going *further* into Components V2 rather than
away from it: the forms message and each thread are one Container with a
separator, which an embed's single description could not do.

**What is still unverified**: whether a bot without the Message Content intent
sees message content in a message-command payload. Discord's documentation does
not say, so an empty body is refused with an explanation rather than posted as
a blank comment.
