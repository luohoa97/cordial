/**
 * The Discord side: open the thread, say what happened, relay the comment.
 *
 * **There is no gateway connection and no privileged intent.** Everything
 * arrives as an interaction over HTTP, which has a consequence worth stating
 * because it shaped the design: the bridge cannot read ordinary messages, so a
 * reply typed into a thread does not become an issue comment on its own.
 * Commenting is a button, which opens a modal.
 *
 * That is not a limitation grudgingly accepted. Reading every message would
 * need the Message Content intent -- privileged, and it means ingesting a
 * channel's whole conversation to catch the parts meant for the tracker. The
 * button costs one click and keeps an issue thread free of "lol" and "same
 * here". The relay in the other direction, GitHub to Discord, is a webhook and
 * needs no intent either.
 */

import { send } from "./resilient.ts";

const API = "https://discord.com/api/v10";

export const EPHEMERAL = 1 << 6;

export const InteractionType = {
  PING: 1,
  APPLICATION_COMMAND: 2,
  MESSAGE_COMPONENT: 3,
  MODAL_SUBMIT: 5,
} as const;

export const ResponseType = {
  PONG: 1,
  MESSAGE: 4,
  DEFERRED_MESSAGE: 5,
  MODAL: 9,
} as const;

export class Discord {
  #token: string;
  #applicationId: string;

  constructor(token: string, applicationId: string) {
    this.#token = token;
    this.#applicationId = applicationId;
  }

  /**
   * `idempotent` says whether an *unknown* outcome may be repeated -- see
   * `resilient.ts`. Posting a message is not: a lost response would double the
   * message. Rate limits are retried either way, which is the case that
   * actually happens, since the bridge makes three or four Discord calls per
   * submission and two reporters at once is enough to meet one.
   */
  async #call(
    method: string,
    path: string,
    body?: unknown,
    idempotent = false,
  ): Promise<Response> {
    const response = await send(
      () =>
        fetch(`${API}${path}`, {
          method,
          headers: {
            "authorization": `Bot ${this.#token}`,
            "content-type": "application/json",
            "user-agent": "cordial-issue-bridge",
          },
          body: body === undefined ? undefined : JSON.stringify(body),
        }),
      `${method} ${path}`,
      { idempotent },
    );
    if (!response.ok) {
      throw new Error(
        `${method} ${path}: Discord answered ${response.status} ${await response.text()}`,
      );
    }
    return response;
  }

  /**
   * Open a thread for an issue, in a forum channel or an ordinary one.
   *
   * The two need different requests -- a forum post carries its opening
   * message in the same call, a text-channel thread is created empty and
   * posted into afterwards -- and which one a channel is cannot be known
   * without asking. Rather than an operator flag that is wrong on the day it
   * matters, this tries the forum shape and falls back. The cost is one
   * rejected request on a text channel; the benefit is one less thing to
   * configure incorrectly.
   */
  async openThread(
    channelId: string,
    name: string,
    opening: string,
    components?: unknown[],
  ): Promise<string> {
    // Components ride on the opening message itself in both shapes, so the
    // controls are in the first post rather than a second one below it -- a
    // forum post's starter message takes them when it is created with the
    // thread, and a text-channel thread's is posted here anyway.
    const message = components
      ? { content: opening, components, flags: 1 << 15 }
      : { content: opening };
    try {
      const forum = await this.#call("POST", `/channels/${channelId}/threads`, {
        name,
        message,
      });
      return (await forum.json()).id;
    } catch {
      const thread = await this.#call("POST", `/channels/${channelId}/threads`, {
        name,
        type: 11,
        auto_archive_duration: 10080,
      });
      const id = (await thread.json()).id;
      await this.post(id, opening, components);
      return id;
    }
  }

  /**
   * A channel's name, which is how a thread says which issue it belongs to.
   *
   * `openThread` names every thread `#<number> <title>`, so the pairing can be
   * read back without touching a single message -- no Message Content intent,
   * and nothing to go stale if somebody edits the opening post.
   */
  async channelName(channelId: string): Promise<string> {
    const response = await this.#call("GET", `/channels/${channelId}`, undefined, true);
    return (await response.json()).name ?? "";
  }

  /**
   * Archive or unarchive a thread.
   *
   * **Archived, never locked.** A locked thread can only be reopened by a
   * moderator, which would strand the reporter the close button exists to
   * serve; an archived one comes back the moment anybody posts in it. Closing
   * an issue should tidy the thread away, not seal it.
   */
  async setArchived(threadId: string, archived: boolean): Promise<void> {
    await this.#call("PATCH", `/channels/${threadId}`, { archived }, true);
  }

  async post(channelId: string, content: string, components?: unknown[]): Promise<void> {
    await this.#call("POST", `/channels/${channelId}/messages`, {
      content,
      ...(components ? { components, flags: 1 << 15 } : {}),
      allowed_mentions: { parse: [] },
    });
  }

  /** Replace the deferred reply to an interaction. */
  async editOriginal(interactionToken: string, body: unknown): Promise<void> {
    // Idempotent: a PATCH replaces the message, so a repeat cannot double
    // anything -- and this is the call that decides whether the reporter sees
    // an answer at all, so it is the one most worth retrying.
    await this.#call(
      "PATCH",
      `/webhooks/${this.#applicationId}/${interactionToken}/messages/@original`,
      body,
      true,
    );
  }
}

/**
 * Pull the answers out of a modal submission.
 *
 * Modal payloads nest: every input sits inside a `Label`, and Discord may wrap
 * things again in future without asking. So this walks the tree for anything
 * carrying a `custom_id` and a value rather than assuming a depth -- a
 * hand-unrolled two-level read is the kind of thing that works until the day
 * the platform adds a container.
 */
export function modalValues(data: unknown): Record<string, string> {
  const found: Record<string, string> = {};
  const walk = (node: unknown): void => {
    if (Array.isArray(node)) {
      for (const child of node) walk(child);
      return;
    }
    if (!node || typeof node !== "object") return;
    const record = node as Record<string, unknown>;
    const id = typeof record.custom_id === "string" ? record.custom_id : null;
    if (id) {
      if (typeof record.value === "string") found[id] = record.value;
      else if (Array.isArray(record.values)) found[id] = record.values.join(", ");
    }
    for (const value of Object.values(record)) {
      if (value && typeof value === "object") walk(value);
    }
  };
  walk(data);
  return found;
}
