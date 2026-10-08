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
export function threadMarker(threadId: string, reporterId?: string, formSlug?: string): string {
  const who = reporterId ? ` reporter=${reporterId}` : "";
  // The form is recorded so an edit knows which headings are the reporter's
  // words. Older issues have none and are matched by their headings instead --
  // see `inferForm` in `edit.ts`.
  const form = formSlug && /^[a-z0-9_-]{1,64}$/i.test(formSlug) ? ` form=${formSlug}` : "";
  return `<!-- ${MARKER} thread=${threadId}${who}${form} -->`;
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

/** The template an issue was filed from, or null for one filed before it was recorded. */
export function formFromBody(body: string | null | undefined): string | null {
  const match = (body ?? "").match(
    new RegExp(`<!--\\s*${MARKER}\\s[^>]*?form=([a-z0-9_-]{1,64})`, "i"),
  );
  return match ? match[1] : null;
}

export function heading(block: FormBlock): string {
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
      `Replies posted here are relayed to them in the thread.` +
      logArchiveNote(form),
  );

  if (threadId) parts.push(threadMarker(threadId, submission.reporter.id, form.slug));
  return parts.join("\n\n");
}

/**
 * What the issue says about the log archive the web form requires.
 *
 * A modal cannot carry a file, so a Discord report never has one attached, and
 * a maintainer reading it should know that is the channel and not an oversight.
 * Empty for a form with no such field.
 */
export function logArchiveNote(form: IssueForm): string {
  if (!form.webOnly.some((b) => b.id === "log-archive")) return "";
  return `\n\nNo log archive is attached: a Discord form cannot carry a file. ` +
    `The reporter can drop the \`.zip\` from Report a Problem → Save logs… into the thread, ` +
    `and a maintainer can add a link to it here with "Add to the issue".`;
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

// ---------------------------------------------------------------------------
// Editing a body that was assembled above.
//
// ## What an edit may touch
//
// A body is the reporter's answers, then a section saying who filed it, then a
// hidden marker the bridge reads back to decide who may close the issue. An
// editor gets the first part and **nothing else**: the `Reported from Discord`
// section and the marker are carried through byte for byte, so editing a report
// cannot reassign it to somebody else, unpair it from its thread or rewrite who
// is credited with it.
//
// The Diagnostics block is also left alone. It is generated by Cordial rather
// than written by the reporter, it is the one thing every form insists on, and
// an editable copy of it is a copy somebody can tidy until it no longer says
// what the install was.
//
// ## Refusing rather than guessing
//
// Parsing a body back into fields is only safe when it came out of
// `renderIssueBody` and nobody has touched it since. `parseBody` therefore
// **re-assembles what it parsed and compares**; if the result is not the body it
// was given -- a maintainer reflowed it on GitHub, a template reordered its
// fields -- it returns null and the caller refuses to edit, because the
// alternative is writing back something subtly different from what was read.
// ---------------------------------------------------------------------------

/** Fields Cordial generated rather than the reporter wrote. Never offered for editing. */
const GENERATED_FIELDS = new Set(["diagnostics"]);

/** The section the bridge adds under the reporter's answers. */
export const REPORTED_HEADING = "### Reported from Discord";

/**
 * Is this field one the reporter wrote, and one a modal can edit?
 *
 * Dropdowns are left out: their value is one of the template's options, and a
 * free-text box would let an edit put a sentence where an option belongs.
 */
export function isEditable(block: FormBlock): boolean {
  return (block.type === "input" || block.type === "textarea") && Boolean(block.id) &&
    !GENERATED_FIELDS.has(block.id!);
}

export interface ParsedSection {
  block: FormBlock;
  /** The section exactly as it stands in the body, truncation note included. */
  raw: string;
  /** The answer, without the heading or the truncation note. */
  value: string;
}

export interface ParsedBody {
  sections: Map<string, ParsedSection>;
  /** The Reported-from-Discord section and marker, verbatim. Empty if there is none. */
  tail: string;
}

const NO_RESPONSE = "_No response_";
const NOTE = /\n\n\*\(This filled Discord's \d+-character limit[\s\S]*\)\*$/;

/** What GitHub's own form renderer writes for an empty optional field counts as empty. */
function answer(raw: string): string {
  const trimmed = raw.trim();
  return trimmed === NO_RESPONSE ? "" : trimmed;
}

/**
 * Split a body into the fields of `form`, or null if that cannot be done safely.
 */
export function parseBody(form: IssueForm, body: string): ParsedBody | null {
  const cut = body.lastIndexOf(`\n\n${REPORTED_HEADING}`);
  const head = cut === -1 ? body : body.slice(0, cut);
  const tail = cut === -1 ? "" : body.slice(cut + 2);

  // A marker inside the editable part would be somebody's text to rewrite,
  // and the marker decides who may act on the issue. Not editable.
  if (head.includes(`<!-- ${MARKER}`)) return null;

  const names = form.fields.map(heading);
  const found: { at: number; index: number }[] = [];
  let next = 0;
  for (const m of head.matchAll(/^### (.*)$/gm)) {
    // Strictly forward through the template, so a `###` line inside somebody's
    // pasted log is content, not a boundary -- unless it is a later field's
    // exact name, which the re-assembly check below catches.
    const index = names.findIndex((n, i) => i >= next && n === m[1].trimEnd());
    if (index !== -1) {
      found.push({ at: m.index!, index });
      next = index + 1;
    }
  }

  const sections = new Map<string, ParsedSection>();
  if (found.length) {
    if (found[0].at !== 0) return null;
    found.forEach(({ at, index }, i) => {
      const end = i + 1 < found.length ? found[i + 1].at : head.length;
      let raw = head.slice(at, end);
      if (i + 1 < found.length) raw = raw.replace(/\n\n$/, "");
      const block = form.fields[index];
      const prefix = `### ${names[index]}`;
      const value = raw.slice(prefix.length).replace(/^\n+/, "").replace(NOTE, "");
      sections.set(block.id!, { block, raw, value });
    });
  } else if (head.length) {
    return null;
  }

  const parsed: ParsedBody = { sections, tail };
  return assembleBody(form, parsed, {}) === body ? parsed : null;
}

/**
 * Put a body back together with some fields replaced.
 *
 * `edits` maps a field id to its new text. A field whose new text matches the
 * old one (ignoring surrounding whitespace) keeps its section **verbatim**, so
 * pressing Edit and saving without changing a box cannot reflow it, drop a
 * truncation note, or turn an `_No response_` into a blank. A new answer in a
 * field that had none is inserted at its place in the template, and an
 * emptied one removes its section.
 */
export function assembleBody(
  form: IssueForm,
  parsed: ParsedBody,
  edits: Record<string, string>,
): string {
  const parts: string[] = [];
  for (const block of form.fields) {
    const id = block.id;
    if (!id) continue;
    const existing = parsed.sections.get(id);
    if (isEditable(block) && id in edits) {
      const next = edits[id];
      if (answer(next) === answer(existing?.value ?? "")) {
        if (existing) parts.push(existing.raw);
        continue;
      }
      if (!next.trim()) continue;
      parts.push(fieldSection(block, next));
    } else if (existing) {
      parts.push(existing.raw);
    }
  }
  if (parsed.tail) parts.push(parsed.tail);
  return parts.join("\n\n");
}

/** Which fields an edit would actually change, with what they were and are. */
export function changedFields(
  parsed: ParsedBody,
  form: IssueForm,
  edits: Record<string, string>,
): { block: FormBlock; before: string; after: string }[] {
  const changed: { block: FormBlock; before: string; after: string }[] = [];
  for (const block of form.fields) {
    const id = block.id;
    if (!id || !isEditable(block) || !(id in edits)) continue;
    const before = answer(parsed.sections.get(id)?.value ?? "");
    const after = answer(edits[id]);
    if (before !== after) changed.push({ block, before, after });
  }
  return changed;
}
