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

const MARKER = "cordial-bridge";

export interface Submission {
  /** Field id to what the user typed or chose. */
  values: Record<string, string>;
  reporter: { id: string; tag: string };
}

export function threadMarker(threadId: string): string {
  return `<!-- ${MARKER} thread=${threadId} -->`;
}

/** The thread id a body was paired with, or null if it was never paired. */
export function threadFromBody(body: string | null | undefined): string | null {
  const match = (body ?? "").match(
    new RegExp(`<!--\\s*${MARKER}\\s+thread=(\\d{1,32})\\s*-->`),
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
export function renderIssueBody(
  form: IssueForm,
  submission: Submission,
  threadId: string | null,
): string {
  const parts: string[] = [];
  for (const block of form.fields) {
    const id = block.id;
    if (!id) continue;
    const value = submission.values[id]?.trim();
    if (!value) continue;
    parts.push(`### ${heading(block)}\n\n${value}`);
  }

  parts.push(
    `### Reported from Discord\n\n` +
      `Filed by **${submission.reporter.tag}** (\`${submission.reporter.id}\`) ` +
      `through Cordial's Discord, which is why the author of this issue is a bot. ` +
      `Replies posted here are relayed to them in the thread.`,
  );

  if (threadId) parts.push(threadMarker(threadId));
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
