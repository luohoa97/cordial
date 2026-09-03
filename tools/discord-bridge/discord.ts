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

  async #call(method: string, path: string, body?: unknown): Promise<Response> {
    const response = await fetch(`${API}${path}`, {
      method,
      headers: {
        "authorization": `Bot ${this.#token}`,
        "content-type": "application/json",
        "user-agent": "cordial-issue-bridge",
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
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
  async openThread(channelId: string, name: string, opening: string): Promise<string> {
    try {
      const forum = await this.#call("POST", `/channels/${channelId}/threads`, {
        name,
        message: { content: opening },
      });
      return (await forum.json()).id;
    } catch {
      const thread = await this.#call("POST", `/channels/${channelId}/threads`, {
        name,
        type: 11,
        auto_archive_duration: 10080,
      });
      const id = (await thread.json()).id;
      await this.post(id, opening);
      return id;
    }
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
    await this.#call(
      "PATCH",
      `/webhooks/${this.#applicationId}/${interactionToken}/messages/@original`,
      body,
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
