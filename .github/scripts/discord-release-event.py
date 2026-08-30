#!/usr/bin/env python3
"""Build the `release` event that releasify reads, trimmed to fit Discord.

Two things are going on here and only one of them is obvious.

The obvious one: `tenedev/releasify-action` reads the release out of
`process.env.GITHUB_EVENT`, and no such environment variable exists. Actions
sets `GITHUB_EVENT_PATH`, a *path*, and the pinned bundle never mentions it.
Left alone the action announces ``v?.?`` with "_No release notes provided._"
and a compare link that 404s -- successfully, with a green tick. The workflow
supplies `GITHUB_EVENT` itself, and this script is what it supplies.

The less obvious one: because we hand the action its input, we can make it fit
limits it does not check. Discord rejects an over-long embed with a 400 and
posts nothing; the action logs the status code and does not fail. So a release
that is merely too long to announce would look exactly like one that was
announced. Cordial's notes are long on purpose -- they carry a what-is-broken
section, which is a rule in AGENTS.md, not an accident -- and measured against
the last five releases, four exceed the description cap on their own:
v0.12.1 5,376 characters, v0.12.0 6,593, v0.11.0 9,657, v0.9.0 5,064.

The asset list overflows separately and for a different reason. Cordial
attaches ten files to a release, five packages and a `.cosign.bundle`
signature beside each, and the action renders every one as a markdown link
into a single embed field. Measured on v0.12.1 that field comes to about 1,600
characters against a 1,024 cap -- so the signatures are dropped here (they are
on the release page, which is where somebody verifying a download is already
looking) and the remainder is trimmed until it fits.

**This file knows how the action formats things, which is coupling, and the
commit pin is what makes that safe.** The workflow pins
bfade1fe75a8a4e8faafbc31c257e44ebfbe8352, so the formatting cannot change
underneath these calculations without somebody editing the pin and reading
this comment on the way past.
"""

from __future__ import annotations

import json
import os
import sys

# Discord's documented caps. The per-embed total is the one that catches
# people out: every individual field can be legal and the embed still rejected.
DESCRIPTION_MAX = 4096
FIELD_VALUE_MAX = 1024
EMBED_TOTAL_MAX = 6000

# Headroom against the total. The action adds a footer, an author name, two
# field names and a title, and this script would rather leave a few hundred
# characters unused than be the reason a release goes unannounced.
SAFETY = 256


def close_open_fence(text: str) -> str:
    """Re-close a code fence that the trim cut through.

    Truncating markdown mid-fence turns the rest of the embed into one code
    block in Discord's renderer, which looks like a formatting bug in the
    release notes rather than in the thing that truncated them.
    """
    if text.count("```") % 2 == 1:
        return text + "\n```"
    return text


def trim_body(body: str, budget: int, url: str) -> str:
    """Cut notes to `budget`, at a line boundary, pointing at the full text."""
    if len(body) <= budget:
        return body

    tail = f"\n\n**[Read the full release notes]({url})**"
    room = budget - len(tail)

    cut = body[:room]
    # Prefer a paragraph break, then any line break, over slicing a sentence
    # -- or worse, a markdown link -- in half.
    for sep in ("\n\n", "\n"):
        at = cut.rfind(sep)
        # Only honour a boundary that is not throwing most of the notes away.
        if at > room * 0.5:
            cut = cut[:at]
            break

    return close_open_fence(cut.rstrip()) + tail


def source_links(assets: list[dict], repo: str, tag: str) -> list[dict]:
    """Drop signatures, then drop assets, until the action's field will fit.

    Mirrors the action's own formatting: every asset as `[name](url)`, joined
    with " | ", with a ZIP and a TAR link of its own appended after them.
    """
    keep = [a for a in assets if not a["name"].endswith(".cosign.bundle")]

    # The two links the action appends itself, counted here because they are
    # part of the same field and therefore part of the same 1,024 characters.
    zip_url = f"https://github.com/{repo}/zipball/{tag}"
    tar_url = f"https://github.com/{repo}/tarball/{tag}"
    fixed = len(f"[ZIP]({zip_url}) | [TAR]({tar_url})")

    def rendered(items: list[dict]) -> int:
        parts = [f"[{a['name']}]({a['browser_download_url']})" for a in items]
        # +3 for the " | " the action puts before the ZIP link.
        return sum(len(p) for p in parts) + 3 * len(parts) + fixed

    while keep and rendered(keep) > FIELD_VALUE_MAX:
        keep.pop()
    return keep


def main() -> int:
    with open(sys.argv[1], encoding="utf-8") as fh:
        release = json.load(fh)

    if not release or not release.get("tag_name"):
        print("::error::no release object to announce", file=sys.stderr)
        return 1

    repo = os.environ.get("GITHUB_REPOSITORY", "luohoa97/cordial")
    tag = release["tag_name"]
    html_url = release.get("html_url") or f"https://github.com/{repo}/releases/tag/{tag}"

    release["assets"] = source_links(release.get("assets") or [], repo, tag)

    # What the rest of the embed will cost, so the notes get the remainder
    # rather than a guessed constant. The title is the action's own default
    # shape; `footer` and `username` come from the workflow.
    overhead = (
        len(f"New Release: `{tag}` in `{repo}`")
        + len("Cordial loads Roblox natively on Linux")
        + len(release.get("author", {}).get("login") or "")
        + len("Source Code")
        + len("Compare Changes")
        + FIELD_VALUE_MAX  # the source-code field, at its worst
        + len(f"[Compare commits](https://github.com/{repo}/compare/main...{tag})")
        + SAFETY
    )
    budget = min(DESCRIPTION_MAX, EMBED_TOTAL_MAX - overhead)

    body = release.get("body") or "_No release notes provided._"
    release["body"] = trim_body(body, budget, html_url)

    # `json.dumps` escapes newlines, so this stays a single line and needs no
    # heredoc delimiter in GITHUB_OUTPUT.
    print("event=" + json.dumps({"release": release}, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    sys.exit(main())
