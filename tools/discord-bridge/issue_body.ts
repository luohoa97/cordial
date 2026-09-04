/**
 * The issue a Discord submission becomes, and how it stays paired to its thread.
 *
 * ## No database, and why that is not a shortcut
 *
 * The bridge needs to know, on a GitHub `issue_comment` webhook, which Discord
 * thread to post into. The usual answer is a table mapping issue numbers to
 * thread ids -- which is a database to provision, migrate, back up and lose.
 *
 * Instead the pairing lives in the two artefacts themselves: the thread id in
 * a hidden HTML comment in the issue body, the issue number in the thread's
 * opening message. GitHub's webhook payload already carries `issue.body`, so
 * reading the pairing costs no request at all, and there is no state that can
 * disagree with reality -- delete the thread and the issue simply stops having
 * one.
 *
 * The marker is an HTML comment because GitHub renders issue bodies as
 * Markdown, where it is invisible, and because a user editing the body around
 * it does not disturb it.
 *
 * ## Attribution
 *
 * A reporter without a GitHub account cannot be the author, so the body names
 * them and links the thread. That is the honest arrangement: the issue says
 * where it came from and where the person who filed it can be reached, rather
 * than appearing to be the bot's own observation.
 */
import type { FormBlock, IssueForm } from "./issue_forms.ts";
import { maxLengthFor } from "./issue_forms.ts";

const MARKER = "cordial-bridge";

export interface Submission {
  /** Field id to what the user typed or chose. */
  values: Record<string, string>;
  reporter: { id: string; tag: string };
}

/**
 * The hidden pairing line: which thread, and who filed it.
 *
 * The reporter's id is here rather than only in the prose above it because
 * something has to *act* on it -- the close button checks the presser against
 * it -- and parsing an identity out of a sentence a maintainer may reword is
 * the kind of thing that works until somebody tidies the wording.
 */
export function threadMarker(threadId: string, reporterId?: string): string {
  const who = reporterId ? ` reporter=${reporterId}` : "";
  return `<!-- ${MARKER} thread=${threadId}${who} -->`;
}

/** The thread id a body was paired with, or null if it was never paired. */
export function threadFromBody(body: string | null | undefined): string | null {
  const match = (body ?? "").match(
    new RegExp(`<!--\\s*${MARKER}\\s+thread=(\\d{1,32})`),
  );
  return match ? match[1] : null;
}

/**
 * The Discord id of whoever filed this, or null.
 *
 * Null for an issue filed on the web, and for one filed by the bridge before
 * the marker carried a reporter -- both must read as "nobody may close this
 * from Discord" rather than as an error.
 */
export function reporterFromBody(body: string | null | undefined): string | null {
  const match = (body ?? "").match(
    new RegExp(`<!--\\s*${MARKER}\\s[^>]*?reporter=(\\d{1,32})`),
  );
  return match ? match[1] : null;
}

function heading(block: FormBlock): string {
  return block.attributes?.label ?? block.id ?? "Field";
}

/**
 * Render the body in the shape GitHub's own form renderer produces -- `###`
 * per field, the answer beneath -- so an issue filed from Discord and one
 * filed from the web read identically in the tracker. A maintainer should not
 * be able to tell which route a report took without looking for the note.
 */
/**
 * One `### Label` section, with a note if the answer ran into Discord's limit.
 *
 * Shared with the follow-up modal's path in `interactions.ts`, which is not a
 * tidiness point: the fields that do not fit the five-component modal are the
 * long ones, so the *overflow* form is where a 4000-character log actually
 * arrives. Issue #28 came in through it. A note wired only into the main body
 * would have missed every case it was written for.
 */
/**
 * A note under any answer that ran into Discord's limit for its field.
 *
 * **A truncated log that does not say it is truncated is a lie the reader
 * cannot see.** Discord's client stops accepting characters at the field's
 * `max_length` without telling the person typing, and it keeps the beginning --
 * so a crash log arrives with the startup banner intact and the crash missing.
 * Issue #28 is the worked example, at 3996 of 4000 characters, ending mid-word
 * on a startup line with the exit status gone.
 *
 * **The test is on the raw value, not the trimmed one, and that is the whole
 * of its precision.** #28's field held exactly 4000 characters and rendered as
 * 3996 once trailing whitespace came off -- so a length check after `trim()`
 * needs a fudge factor, and any fudge factor either misses a real truncation
 * or annotates an answer that merely came close. Discord returns the box's
 * contents verbatim, so a box that is full is exactly `max_length` long and
 * there is nothing to estimate.
 */
export function fieldSection(block: FormBlock, raw: string): string {
  return `### ${heading(block)}\n\n${raw.trim()}${truncationNote(block, raw)}`;
}

function truncationNote(block: FormBlock, raw: string): string {
  if (raw.length < maxLengthFor(block.type)) return "";
  return `\n\n*(This filled Discord's ${maxLengthFor(block.type)}-character limit for one ` +
    `field, so it is the **beginning** of what was pasted and the end is missing. If the ` +
    `end is the part that matters -- it usually is, for a crash -- post it in the thread ` +
    `and use **Add to the issue**.)*`;
}

export function renderIssueBody(
  form: IssueForm,
  submission: Submission,
  threadId: string | null,
): string {
  const parts: string[] = [];
  for (const block of form.fields) {
    const id = block.id;
    if (!id) continue;
    const raw = submission.values[id];
    if (!raw?.trim()) continue;
    parts.push(fieldSection(block, raw));
  }

  parts.push(
    `### Reported from Discord\n\n` +
      `Filed by **${submission.reporter.tag}** (\`${submission.reporter.id}\`) ` +
      `through Cordial's Discord, which is why the author of this issue is a bot. ` +
      `Replies posted here are relayed to them in the thread.`,
  );

  if (threadId) parts.push(threadMarker(threadId, submission.reporter.id));
  return parts.join("\n\n");
}

/**
 * The issue title.
 *
 * A form with a `title:` prefix keeps it, so `bug_report`'s issues still read
 * `[Bug]: ...` whichever side they came from. The rest of the line is the
 * first required answer, trimmed to one line -- there is no title field in a
 * modal to spare, and five components are all there are.
 */
export function renderIssueTitle(form: IssueForm, submission: Submission): string {
  const first = form.fields.find(
    (b) => b.validations?.required && b.id && submission.values[b.id]?.trim(),
  );
  const raw = first?.id ? submission.values[first.id] : "";
  const line = raw.split("\n").map((s) => s.trim()).find(Boolean) ?? form.name;
  const limit = 120 - form.titlePrefix.length;
  const body = line.length > limit ? line.slice(0, limit - 1).trimEnd() + "…" : line;
  return `${form.titlePrefix}${body}`;
}
