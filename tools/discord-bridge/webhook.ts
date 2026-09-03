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
  issue?: { number?: number; body?: string | null; html_url?: string };
  comment?: { body?: string; html_url?: string; user?: { login?: string; type?: string } };
}

export interface Relay {
  threadId: string;
  content: string;
}

/**
 * What to post in the thread for this event, or null to ignore it.
 *
 * Pure, so the decisions -- which events relay, which are the bridge's own
 * echo, what an unpaired issue does -- are testable without a network.
 */
export function relayFor(event: CommentEvent, selfLogin: string): Relay | null {
  if (event.action !== "created") return null;
  const login = event.comment?.user?.login ?? "";
  if (login === selfLogin || login === `${selfLogin}[bot]`) return null;

  const body = event.issue?.body ?? "";
  const match = body.match(/<!--\s*cordial-bridge\s+thread=(\d{1,32})\s*-->/);
  if (!match) return null;

  const text = (event.comment?.body ?? "").trim();
  if (!text) return null;

  // Trimmed rather than split across messages: a long comment belongs on the
  // issue, and the thread's job is to tell the reporter something was said and
  // where to read it.
  const limit = 1500;
  const shown = text.length > limit ? text.slice(0, limit - 1).trimEnd() + "…" : text;
  return {
    threadId: match[1],
    content: `**${login}** commented on #${event.issue?.number}:\n\n${shown}` +
      (event.comment?.html_url ? `\n\n<${event.comment.html_url}>` : ""),
  };
}
