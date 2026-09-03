/**
 * What each button press and form submission does.
 *
 * ## Why almost everything defers
 *
 * Discord gives an interaction **three seconds** to be acknowledged. Filing an
 * issue is two or three GitHub calls and opening a thread is another, so
 * answering a modal submit directly is a race that is lost on a slow day and
 * won in testing -- the worst shape of bug. Every path that touches the
 * network therefore acknowledges immediately with a deferred, ephemeral reply
 * and edits it when the work is done.
 *
 * Opening a modal is the exception and must **not** defer: a modal is only a
 * valid first response to an interaction, so deferring loses the ability to
 * show one at all. That is also why the follow-up for a form's leftover
 * optional fields is a button rather than a second modal chained off the
 * first: a modal submit cannot itself open a modal, but the message it leaves
 * behind can carry a button that does.
 */
import { Discord, EPHEMERAL, InteractionType, modalValues, ResponseType } from "./discord.ts";
import { ACTION_ROW, BUTTON, type IssueForm, modalFor } from "./issue_forms.ts";
import { GitHub } from "./github.ts";
import {
  renderIssueBody,
  renderIssueTitle,
  reporterFromBody,
  type Submission,
} from "./issue_body.ts";

export interface Context {
  forms: () => Promise<IssueForm[]>;
  github: GitHub;
  discord: Discord;
  threadChannelId: string;
  repoUrl: string;
}

interface ResolvedMessage {
  content?: string;
  author?: { username?: string; global_name?: string; bot?: boolean };
  id?: string;
}

interface Interaction {
  type: number;
  token: string;
  channel_id?: string;
  guild_id?: string;
  data?: {
    custom_id?: string;
    name?: string;
    type?: number;
    target_id?: string;
    resolved?: { messages?: Record<string, ResolvedMessage> };
  };
  member?: {
    user?: { id: string; username: string; global_name?: string };
    /** A decimal bitfield string, as Discord sends it. */
    permissions?: string;
  };
  user?: { id: string; username: string; global_name?: string };
}

function reporter(interaction: Interaction): Submission["reporter"] {
  const user = interaction.member?.user ?? interaction.user;
  return {
    id: user?.id ?? "unknown",
    tag: user?.global_name ?? user?.username ?? "someone",
  };
}

const deferred = () => ({
  type: ResponseType.DEFERRED_MESSAGE,
  data: { flags: EPHEMERAL },
});

/**
 * Make sure a deferred interaction always gets an answer.
 *
 * **A deferred reply that is never edited is a spinner that never stops.**
 * Discord shows "… is thinking" until something replaces it, and until now any
 * throw inside a follow-up left exactly that -- the reporter waiting on a
 * message that was never coming, with no way to tell whether their report
 * landed. It is the same symptom whatever the cause, which is what made the
 * `waitUntil` bug so opaque.
 *
 * So every follow-up is wrapped: on failure it edits the reply to say so. The
 * interaction token is good for fifteen minutes, so there is no hurry and no
 * excuse.
 */
function reporting(
  context: Context,
  token: string,
  work: () => Promise<void>,
): () => Promise<void> {
  return async () => {
    try {
      await work();
    } catch (error) {
      const why = error instanceof Error ? error.message : String(error);
      console.error(`follow-up: ${why}`);
      try {
        await context.discord.editOriginal(token, {
          content: "That did not work, and nothing was filed. You can try again — " +
            "and if it keeps happening, this is worth reporting on GitHub directly:\n" +
            `\`\`\`\n${why.slice(0, 600)}\n\`\`\``,
          components: [],
        });
      } catch (second) {
        // Both the work and the apology failed. Nothing further can reach the
        // user, so this is the end of the line; log it where a maintainer can
        // find it.
        console.error(`could not even report the failure: ${second}`);
      }
    }
  };
}

const ephemeral = (content: string) => ({
  type: ResponseType.MESSAGE,
  data: { content, flags: EPHEMERAL },
});

/**
 * Handle one interaction.
 *
 * Returns the immediate response body. Anything slower is done in `after`,
 * which the caller runs without holding the response -- the three-second
 * budget is spent by the time it is called.
 */
export async function handle(
  context: Context,
  interaction: Interaction,
): Promise<{ response: unknown; after?: () => Promise<void> }> {
  if (interaction.type === InteractionType.PING) {
    return { response: { type: ResponseType.PONG } };
  }

  const id = interaction.data?.custom_id ?? "";
  const [verb, ...rest] = id.split(":");
  const forms = await context.forms();
  const find = (slug: string) => forms.find((f) => f.slug === slug);

  if (interaction.type === InteractionType.APPLICATION_COMMAND) {
    if (interaction.data?.name === "Add to the issue") {
      return {
        response: deferred(),
        after: reporting(
          context,
          interaction.token,
          () => addMessageToIssue(context, interaction),
        ),
      };
    }
    return { response: ephemeral("That command is not one this bot knows about.") };
  }

  if (interaction.type === InteractionType.MESSAGE_COMPONENT) {
    if (verb === "cordial-issue-open") {
      const form = find(rest[0]);
      if (!form) return { response: ephemeral(unknownForm(rest[0])) };
      return { response: { type: ResponseType.MODAL, data: modalFor(form, "main") } };
    }
    if (verb === "cordial-issue-extra") {
      const form = find(rest[0]);
      if (!form || !form.dropped.length) {
        return { response: ephemeral("There is nothing more to add for this form.") };
      }
      const modal = modalFor(form, "extra") as Record<string, unknown>;
      // The issue number rides in the custom_id, because the submit that
      // follows arrives as a fresh interaction with no memory of this one and
      // there is deliberately nowhere to keep it.
      modal.custom_id = `cordial-issue:${form.slug}:extra:${rest[1]}`;
      return { response: { type: ResponseType.MODAL, data: modal } };
    }
    if (verb === "cordial-close" || verb === "cordial-reopen" || verb === "cordial-fixed") {
      const open = verb === "cordial-reopen";
      const completed = verb === "cordial-fixed";
      if (completed && !canSayItIsFixed(interaction)) {
        return {
          response: ephemeral(
            "Only someone who helps run this server can mark an issue completed — " +
              "that is a claim that it was fixed. If you filed it and simply do not " +
              "need it any more, use **Close it**.",
          ),
        };
      }
      return {
        response: deferred(),
        after: reporting(
          context,
          interaction.token,
          () =>
            setIssueOpen(
              context,
              interaction,
              Number(rest[0]),
              reporter(interaction),
              open,
              completed,
            ),
        ),
      };
    }

    if (verb === "cordial-comment") {
      return {
        response: {
          type: ResponseType.MODAL,
          data: {
            custom_id: `cordial-comment:${rest[0]}`,
            title: `Comment on #${rest[0]}`.slice(0, 45),
            components: [{
              type: 18,
              label: "Your comment",
              description: "Posted on the issue, with your Discord name against it.",
              component: {
                type: 4,
                custom_id: "comment",
                style: 2,
                required: true,
                max_length: 4000,
              },
            }],
          },
        },
      };
    }
  }

  if (interaction.type === InteractionType.MODAL_SUBMIT) {
    const values = modalValues(interaction.data);
    const who = reporter(interaction);

    if (verb === "cordial-issue" && rest[1] === "main") {
      const form = find(rest[0]);
      if (!form) return { response: ephemeral(unknownForm(rest[0])) };
      return {
        response: deferred(),
        after: reporting(
          context,
          interaction.token,
          () => fileIssue(context, interaction, form, { values, reporter: who }),
        ),
      };
    }

    if (verb === "cordial-issue" && rest[1] === "extra") {
      const form = find(rest[0]);
      const number = Number(rest[2]);
      if (!form || !Number.isInteger(number)) {
        return { response: ephemeral("That form is no longer available.") };
      }
      const extra = form.dropped
        .filter((b) => b.id && values[b.id]?.trim())
        .map((b) => `### ${b.attributes?.label ?? b.id}\n\n${values[b.id!].trim()}`)
        .join("\n\n");
      if (!extra) return { response: ephemeral("Nothing was filled in, so nothing was added.") };
      return {
        response: deferred(),
        after: reporting(context, interaction.token, async () => {
          await context.github.comment(number, `${extra}\n\n*Added by ${who.tag} from Discord.*`);
          await context.discord.editOriginal(interaction.token, {
            content: `Added to [#${number}](${context.repoUrl}/issues/${number}).`,
          });
        }),
      };
    }

    if (verb === "cordial-comment") {
      const number = Number(rest[0]);
      const text = values.comment?.trim();
      if (!Number.isInteger(number) || !text) {
        return { response: ephemeral("Nothing to post.") };
      }
      return {
        response: deferred(),
        after: reporting(context, interaction.token, async () => {
          await context.github.comment(number, `**${who.tag}** (from Discord):\n\n${text}`);
          await context.discord.editOriginal(interaction.token, {
            content: `Posted on [#${number}](${context.repoUrl}/issues/${number}).`,
          });
        }),
      };
    }
  }

  return { response: ephemeral("That control is not one this bot knows about.") };
}

/**
 * Discord permission bits that mean "this person helps run the place".
 *
 * Used only to gate **Mark as completed**, which is a claim about the project
 * rather than about the reporter's own intent -- "this is fixed" is not
 * something the person who reported it gets to assert on everyone's behalf,
 * and a tracker where it is stops being able to answer what was actually
 * fixed.
 */
const MANAGE_MESSAGES = 1n << 13n;
const MANAGE_THREADS = 1n << 34n;
const ADMINISTRATOR = 1n << 3n;

function canSayItIsFixed(interaction: Interaction): boolean {
  const bits = interaction.member?.permissions;
  if (!bits) return false;
  let held: bigint;
  try {
    held = BigInt(bits);
  } catch {
    return false;
  }
  return (held & (MANAGE_MESSAGES | MANAGE_THREADS | ADMINISTRATOR)) !== 0n;
}

/** The controls that sit on an issue thread's first message. */
function threadControls(number: number): unknown[] {
  return [{
    type: ACTION_ROW,
    components: [
      {
        type: BUTTON,
        style: 2,
        label: "Comment on the issue",
        custom_id: `cordial-comment:${number}`,
      },
      { type: BUTTON, style: 3, label: "Mark as completed", custom_id: `cordial-fixed:${number}` },
      { type: BUTTON, style: 4, label: "Close it", custom_id: `cordial-close:${number}` },
      { type: BUTTON, style: 2, label: "Reopen it", custom_id: `cordial-reopen:${number}` },
    ],
  }];
}

function unknownForm(slug: string): string {
  return `There is no form called \`${slug}\` any more. The buttons above may be ` +
    `out of date — ask a maintainer to repost them.`;
}

/**
 * File the issue, open its thread, and pair the two.
 *
 * The order matters and is the reason for the extra `PATCH`: the thread's
 * opening message quotes the issue number, so the issue must exist first --
 * and the issue body carries the thread id, which does not exist until after
 * that. Whichever way round, one of them is written twice. Doing it in this
 * order means a failure half way leaves a complete issue with no thread rather
 * than a thread pointing at nothing.
 */
async function fileIssue(
  context: Context,
  interaction: { token: string },
  form: IssueForm,
  submission: Submission,
): Promise<void> {
  const title = renderIssueTitle(form, submission);
  const issue = await context.github.createIssue(
    title,
    renderIssueBody(form, submission, null),
    form.labels,
  );

  let threadId: string | null = null;
  try {
    threadId = await context.discord.openThread(
      context.threadChannelId,
      `#${issue.number} ${title}`.slice(0, 100),
      `**${title}**\n${issue.html_url}\n\nFiled by ${submission.reporter.tag}. ` +
        `Comments on the issue appear here. To add something, right-click a message ` +
        `→ Apps → "Add to the issue".`,
      threadControls(issue.number),
    );
    await context.github.setIssueBody(
      issue.number,
      renderIssueBody(form, submission, threadId),
    );
  } catch (error) {
    // An issue with no thread is a worse report, not a lost one. Say so rather
    // than reporting a success that has half happened.
    console.error(`thread for #${issue.number}: ${error}`);
  }

  const more = form.dropped.length
    ? [{
      type: ACTION_ROW,
      components: [{
        type: BUTTON,
        style: 2,
        label: "Add the rest",
        custom_id: `cordial-issue-extra:${form.slug}:${issue.number}`,
      }],
    }]
    : [];

  await context.discord.editOriginal(interaction.token, {
    content: `Filed as [#${issue.number}](${issue.html_url})` +
      (threadId ? ` — follow it in <#${threadId}>.` : ", but the thread could not be opened.") +
      (form.dropped.length
        ? `\n\nThis form has ${form.dropped.length} more optional field(s) that did not ` +
          `fit in one dialog. They help, and you can skip them.`
        : ""),
    components: more,
  });
}

/**
 * Put one chosen message on the issue its thread belongs to.
 *
 * The issue number comes from the **thread's name** rather than from reading
 * any message: `openThread` names every thread `#<number> <title>`, so the
 * pairing survives without a Message Content intent and without depending on
 * an opening post nobody may have left alone.
 *
 * **The message body may legitimately be empty**, and the code says so rather
 * than posting a blank comment. An attachment-only message has no content, and
 * Discord's documentation does not state whether a bot without the intent sees
 * content in a message-command payload -- so this degrades visibly instead of
 * assuming, and the reporter is told what happened either way.
 */
async function addMessageToIssue(context: Context, interaction: Interaction): Promise<void> {
  const say = (content: string) => context.discord.editOriginal(interaction.token, { content });

  const channel = interaction.channel_id;
  if (!channel) return await say("This has to be used inside an issue thread.");

  const name = await context.discord.channelName(channel);
  const number = Number(name.match(/^#(\d+)\b/)?.[1]);
  if (!Number.isInteger(number)) {
    return await say(
      "This does not look like an issue thread — its name does not start with " +
        "`#<number>`, which is how the bridge finds the issue. Use it in a thread " +
        "the bot opened.",
    );
  }

  const target = interaction.data?.target_id;
  const message = target ? interaction.data?.resolved?.messages?.[target] : undefined;
  const text = (message?.content ?? "").trim();
  if (!text) {
    return await say(
      `Nothing to add: that message has no text the bot can read. If it was an ` +
        `attachment or an embed, quote the part that matters in a reply and add ` +
        `that instead.`,
    );
  }

  const author = message?.author?.global_name ?? message?.author?.username ?? "someone";
  const link = interaction.guild_id && target
    ? `\n\n<https://discord.com/channels/${interaction.guild_id}/${channel}/${target}>`
    : "";
  await context.github.comment(
    number,
    `**${author}** in Discord:\n\n${text}${link}`,
  );
  await say(`Added to [#${number}](${context.repoUrl}/issues/${number}).`);
}

/**
 * Let the person who filed an issue close it again.
 *
 * They cannot close it on GitHub -- having no account there is the whole
 * reason the bridge exists -- so filing without closing is half a permission.
 *
 * **Who is allowed is read from the issue, not from the button.** A custom_id
 * is client-supplied and anybody who can see the message can press it; the
 * reporter's id lives in the issue body, which only the App can write. So the
 * check is against the tracker's own record every time, and an issue filed on
 * the web (no reporter in its marker) can never be closed from Discord at all.
 *
 * This does not take anything away from maintainers: closing as `not_planned`
 * is an ordinary close and reopening is one click on GitHub.
 */
async function setIssueOpen(
  context: Context,
  interaction: { token: string; channel_id?: string },
  number: number,
  who: Submission["reporter"],
  open: boolean,
  completed = false,
): Promise<void> {
  const say = (content: string) => context.discord.editOriginal(interaction.token, { content });
  const link = `[#${number}](${context.repoUrl}/issues/${number})`;

  if (!Number.isInteger(number)) return await say("That button has lost its issue number.");

  const issue = await context.github.issue(number);
  if ((issue.state === "open") === open) {
    return await say(`${link} is already ${open ? "open" : "closed"}.`);
  }

  // Marking something fixed is a maintainer's call and was already gated on
  // Discord permissions, so it does not also have to be the reporter's issue.
  if (!completed) {
    const reporterId = reporterFromBody(issue.body);
    if (!reporterId) {
      return await say(
        `${link} was not filed from Discord, so it cannot be changed from here. ` +
          `Ask a maintainer, or use GitHub.`,
      );
    }
    if (reporterId !== who.id) {
      return await say(
        `Only the person who filed this can ${open ? "reopen" : "close"} it from Discord. ` +
          `A maintainer can do it on GitHub.`,
      );
    }
  }

  const what = completed ? "Marked completed" : open ? "Reopened" : "Closed";
  await context.github.comment(
    number,
    `${what} by **${who.tag}** from Discord.`,
  );
  await context.github.setIssueOpen(number, open, completed);

  // The thread follows the issue: tidied away when it is closed, back when it
  // is not. Unarchiving first, because a message cannot be posted into an
  // archived thread.
  const thread = interaction.channel_id;
  if (thread) {
    try {
      if (open) await context.discord.setArchived(thread, false);
      await context.discord.post(
        thread,
        open
          ? `**Reopened** by ${who.tag}. ${context.repoUrl}/issues/${number}`
          : `**${what}** by ${who.tag}. Posting here brings the thread back if it turns ` +
            `out not to be finished.`,
      );
      if (!open) await context.discord.setArchived(thread, true);
    } catch (error) {
      // The issue is the record; the thread is where it is discussed. Failing
      // to tidy the thread must not undo a close that already happened.
      console.error(`thread for #${number}: ${error}`);
    }
  }

  await say(
    open ? `Reopened ${link}.` : `${what} ${link} and archived the thread. Thanks for saying so.`,
  );
}
