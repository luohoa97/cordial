# The Discord issue bridge

People report bugs in Discord and not on the tracker, and it is not laziness: filing an issue needs
a GitHub account, and somebody who has one still has to leave the conversation they were already in.
So reports arrive as chat, get answered once, and are gone.

This bridge lets them file properly from Discord. A pinned message offers one button per issue
template; pressing one opens a Discord modal carrying that template's fields; submitting it creates
the issue, opens a thread, and links the two.

From then on the thread is the issue's other face:

| In Discord                                          | What happens                                           |
| --------------------------------------------------- | ------------------------------------------------------ |
| **Comment on the issue**                            | A modal; the text becomes a comment, attributed to you |
| Right-click a message → Apps → **Add to the issue** | That one message becomes a comment                     |
| **Close it**                                        | Closes as _not planned_, and archives the thread       |
| **Reopen it**                                       | Reopens, and brings the thread back                    |
| **Mark as completed**                               | Closes as _completed_. Maintainers only                |

And in the other direction: a comment on the issue appears in the thread, and closing or reopening
it on GitHub moves the thread to match.

**Close and complete are separate on purpose.** "I do not need this any more" is the reporter's to
say and closes as `not_planned`; "this is fixed" is a claim about the project and closes as
`completed`. A tracker where those look alike cannot answer what was actually fixed. So the first is
checked against the reporter recorded in the issue, and the second against Discord permissions — and
a maintainer may complete an issue they did not file.

Who may close is read from **the issue**, never from the button. A `custom_id` is client-supplied
and anyone who can see the message can press it, so the reporter's id lives in the issue body where
only the App can write it. An issue filed on the web has no reporter recorded and cannot be closed
from Discord at all.

**The forms are generated from `.github/ISSUE_TEMPLATE/`, never hand-copied.** That is the whole
design constraint: `config.yml` sets `blank_issues_enabled: false` on purpose, because the required
Diagnostics block lives in the forms, and a bridge that posted chat messages as issues would reverse
that silently. See [ADR-030](../../docs/adr/ADR-030-reports-arrive-from-discord.md).

## What it is not

**It does not read your messages.** There is no gateway connection and no Message Content intent.
Everything arrives as an interaction over HTTP, which is why commenting is a button rather than
"type in the thread" — reading every message would mean ingesting a channel's whole conversation to
catch the parts meant for the tracker, and it would fill issues with "same here". The right-click
command is the deliberate middle: one message, chosen by a person, delivered as an interaction.

**The bot shows as offline, and that is correct.** Discord marks a bot online only while it holds a
gateway socket. This one never does; interactions arrive as HTTP POSTs, which is a different path
entirely. A grey dot beside working buttons is the expected shape here, not a fault.

**Messages are Components V2.** `content` and `embeds` are refused alongside the `IS_COMPONENTS_V2`
flag — a message is one shape or the other — so text is a Text Display and the forms message and
each thread are a Container with a separator. Mixing the two is what stopped every thread from
opening for one release; `components_test.ts` pins the invariant.

**It has no database.** The issue-to-thread pairing lives in the two artefacts: the thread id in a
hidden HTML comment in the issue body, the issue number in the thread's opening message. GitHub's
webhook payload already carries `issue.body`, so reading the pairing costs no request, and there is
no state to lose or migrate.

## Running the checks

```bash
cd tools/discord-bridge
npm ci             # the Worker's one dependency; Deno resolves it from here too
deno task check    # do the templates still fit a five-component modal?
deno task test     # 79 tests, no network, no credentials
```

`npm ci` comes first: `yaml` is an npm dependency so the same import resolves under Cloudflare
Workers' bundler, and Deno reads it out of `node_modules`, so a `deno task` before it fails inside
the dependency rather than in our code.

`deno task check` belongs in CI. The bridge fetches templates at runtime so an edit needs no
redeploy — which moves "this template no longer fits" from CI to a user pressing a button, so the
runtime keeps serving the last good set and says so, and the check is how you find out first.

## Setting it up

```bash
cd tools/discord-bridge
deno task setup
```

It asks for each value, checks every one against the live API before writing anything, and writes
`.env` at `0600`. Secrets are read without echo and never printed back. If anything is wrong it
names the setting and what the API said, and writes nothing.

**Give the bridge its own application.** Cordial already has one — `1543200871767212062`, which
`plugins/discord-presence/main.ts` publishes every user's "Playing Cordial" under. Setup rewrites
the application description, which is public, and one application carrying both means a problem with
the bot is a problem with every user's presence. The script refuses that id by name.

It does **not** touch the application's icon: the bot's face is the bot user's avatar
(`PATCH /users/@me`) and the Rich Presence artwork is the application's icon
(`PATCH /applications/@me`). They are separate pictures, and an earlier version of this script
conflated them — which would have replaced Cordial's presence artwork with the bot's googly eyes for
every user.

**Two steps are not automatable and no script can make them so.** Discord has no endpoint that
creates an application — checked against their resource documentation on 2026-09-03; there is a get
and an edit and nothing that makes one — and no Dynamic Client Registration either. So creating the
application and its bot, and copying the token once, are yours. Everything after that the setup
script does: it sets the interactions endpoint URL, uploads the avatar, sets the description, and
prints the invite link.

`.env` is gitignored. That is real and it is not complete: it is plaintext, so it is as safe as the
machine it is on, and the bot token in it is enough to be the bot. Rotate from the portal if it ever
leaves.

## What has to exist before it works

Six things, and none of them are optional.

**A Discord application** with a bot user. From its page you need the **application id**, the
**public key** and a **bot token**. Set the Interactions Endpoint URL to
`https://your-host/interactions` — Discord will immediately send a signed ping and refuse the URL if
the answer is wrong, which is a useful first test.

**A GitHub App**, installed on the repository, with **Issues: read and write**. You need its **app
id**, a **private key**, and the **installation id** (the number at the end of the installation's
settings URL). The private key works in either encoding GitHub or `openssl` hands you.

**A webhook** on that App for **`issue_comment` and `issues`** events, pointing at
`https://your-host/github`, with a secret. `issues` is what carries a close or a reopen back into
the thread; without it the Discord side never learns an issue was finished. Neither the event list
nor the webhook's Active flag has an API — both are set in the App's settings page.

Then:

| Variable                      | What                                                       |
| ----------------------------- | ---------------------------------------------------------- |
| `DISCORD_APPLICATION_ID`      | From the application page                                  |
| `DISCORD_PUBLIC_KEY`          | From the application page; verifies every request          |
| `DISCORD_BOT_TOKEN`           | The bot's token                                            |
| `DISCORD_THREAD_CHANNEL_ID`   | Where issue threads are opened                             |
| `DISCORD_PICKER_CHANNEL_ID`   | Where the form message is posted                           |
| `GITHUB_OWNER`, `GITHUB_REPO` | The tracker                                                |
| `GITHUB_APP_ID`               | The App's id                                               |
| `GITHUB_APP_PRIVATE_KEY`      | The PEM, whole, newlines included                          |
| `GITHUB_INSTALLATION_ID`      | The App's installation on that repository                  |
| `GITHUB_WEBHOOK_SECRET`       | Must match the webhook's                                   |
| `GITHUB_APP_LOGIN`            | The bot's login, so its own comments are not echoed back   |
| `GITHUB_READ_TOKEN`           | Optional. Only raises the rate limit for reading templates |
| `GITHUB_REF_NAME`             | Optional, defaults to `main`                               |

**`GITHUB_APP_LOGIN` is the one that fails quietly if it is wrong.** It is how the bridge recognises
its own comments; set it wrong and a comment filed from Discord is relayed back into the thread it
came from.

Then:

```bash
deno task serve                      # locally, on $PORT (default 8000)
deno task post-picker --dry-run      # see the message without sending it
deno task post-picker                # post it, then pin it
deno task register-commands          # the right-click "Add to the issue" command
```

`register-commands` uploads the context-menu command. `PUT` replaces the whole set, so a command
deleted from `commands.ts` disappears from Discord rather than lingering as a button that errors.

Repost the picker after adding or renaming a template. Buttons on an old message keep working for
forms that still exist, and say so politely for one that does not.

## Deploying

It is one stateless handler with two routes, so anywhere that serves a request will do. On Deno
Deploy, point the entry at `main.ts` and set the variables above; there is nothing else to
provision.

## What is not verified

**None of this has been run against a real Discord server or a real GitHub App.** The component
rules are read from Discord's reference of 2026-09-03, the modals are generated and inspected, the
signature checks are tested against keys generated in the test and against `openssl` as an
independent oracle, and the whole interaction flow is exercised with fakes. That is a long way from
watching somebody file an issue from Discord, and the first real run should be treated as the first
real run.
