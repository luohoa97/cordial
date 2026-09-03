/**
 * GitHub's side of the relay: a comment on the issue becomes a message in the
 * thread.
 *
 * This direction needs no Discord intent and no state. The webhook payload
 * carries `issue.body`, the thread id is in it, so the pairing is read out of
 * the event itself.
 *
 * **The loop has to be cut explicitly.** A comment the bridge posted on behalf
 * of somebody in Discord arrives back here as an `issue_comment` event, and
 * relaying it would put the message in the thread it came from. Comments
 * authored by the App's own bot are therefore dropped -- checked by the
 * sender's login rather than by looking for a marker in the text, because a
 * marker is something a person can type.
 */

/** Constant-time, because a signature check that leaks by timing is not one. */
function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let difference = 0;
  for (let i = 0; i < a.length; i++) difference |= a[i] ^ b[i];
  return difference === 0;
}

export async function verifyGitHubSignature(
  secret: string,
  header: string | null,
  body: string,
): Promise<boolean> {
  if (!header?.startsWith("sha256=")) return false;
  const expected = header.slice("sha256=".length);
  if (!/^[0-9a-f]{64}$/.test(expected)) return false;

  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const mac = new Uint8Array(
    await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(body)),
  );
  const got = [...mac].map((b) => b.toString(16).padStart(2, "0")).join("");
  return sameBytes(new TextEncoder().encode(got), new TextEncoder().encode(expected));
}

export interface CommentEvent {
  action?: string;
  issue?: {
    number?: number;
    body?: string | null;
    html_url?: string;
    state_reason?: string | null;
  };
  comment?: { body?: string; html_url?: string; user?: { login?: string; type?: string } };
  sender?: { login?: string };
}

export interface Relay {
  threadId: string;
  content: string;
  /**
   * What the thread should do afterwards: follow the issue closed, or come
   * back open. Undefined leaves it alone, which is right for a comment.
   */
  archive?: boolean;
}

/** The thread id paired to an issue body, or null. */
function threadOf(body: string | null | undefined): string | null {
  return (body ?? "").match(/<!--\s*cordial-bridge\s+thread=(\d{1,32})/)?.[1] ?? null;
}

/**
 * What to post in the thread for this event, or null to ignore it.
 *
 * Pure, so the decisions -- which events relay, which are the bridge's own
 * echo, what an unpaired issue does -- are testable without a network.
 */
export function relayFor(
  event: CommentEvent,
  selfLogin: string,
  eventName = "issue_comment",
): Relay | null {
  if (eventName === "issues") return stateRelay(event, selfLogin);
  if (eventName !== "issue_comment") return null;
  if (event.action !== "created") return null;
  const login = event.comment?.user?.login ?? "";
  if (login === selfLogin || login === `${selfLogin}[bot]`) return null;

  const match = threadOf(event.issue?.body);
  if (!match) return null;

  const text = (event.comment?.body ?? "").trim();
  if (!text) return null;

  // Trimmed rather than split across messages: a long comment belongs on the
  // issue, and the thread's job is to tell the reporter something was said and
  // where to read it.
  const limit = 1500;
  const shown = text.length > limit ? text.slice(0, limit - 1).trimEnd() + "…" : text;
  return {
    threadId: match,
    content: `**${login}** commented on #${event.issue?.number}:\n\n${shown}` +
      (event.comment?.html_url ? `\n\n<${event.comment.html_url}>` : ""),
  };
}

/**
 * A close or reopen on GitHub, mirrored into the thread.
 *
 * The maintainer who fixes something works on GitHub, and until this existed
 * the reporter in Discord had no way of learning that their issue was done
 * short of opening the link. The thread follows the issue: archived when it
 * closes, back when it reopens.
 *
 * **The bridge's own state changes are dropped**, because closing from the
 * Discord button already says so in the thread; relaying the webhook it causes
 * would say it twice. Recognised by the sender's login rather than by a marker
 * in the text, since a marker is something a person can type.
 */
function stateRelay(event: CommentEvent, selfLogin: string): Relay | null {
  const open = event.action === "reopened";
  if (event.action !== "closed" && !open) return null;

  const login = event.sender?.login ?? "";
  if (login === selfLogin || login === `${selfLogin}[bot]`) return null;

  const threadId = threadOf(event.issue?.body);
  if (!threadId) return null;

  const number = event.issue?.number;
  const fixed = event.issue?.state_reason === "completed";
  const what = open ? "reopened" : fixed ? "closed as completed" : "closed";
  return {
    threadId,
    content: `**${login}** ${what} #${number} on GitHub.` +
      (open
        ? ""
        : fixed
        ? " If it is still happening for you, say so here and it can be reopened."
        : " Say so here if that is wrong.") +
      (event.issue?.html_url ? `\n<${event.issue.html_url}>` : ""),
    archive: !open,
  };
}
