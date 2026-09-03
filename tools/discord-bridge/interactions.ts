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
import { renderIssueBody, renderIssueTitle, type Submission } from "./issue_body.ts";

export interface Context {
  forms: () => Promise<IssueForm[]>;
  github: GitHub;
  discord: Discord;
  threadChannelId: string;
  repoUrl: string;
}

interface Interaction {
  type: number;
  token: string;
  data?: { custom_id?: string };
  member?: { user?: { id: string; username: string; global_name?: string } };
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
        after: () => fileIssue(context, interaction, form, { values, reporter: who }),
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
        after: async () => {
          await context.github.comment(number, `${extra}\n\n*Added by ${who.tag} from Discord.*`);
          await context.discord.editOriginal(interaction.token, {
            content: `Added to [#${number}](${context.repoUrl}/issues/${number}).`,
          });
        },
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
        after: async () => {
          await context.github.comment(number, `**${who.tag}** (from Discord):\n\n${text}`);
          await context.discord.editOriginal(interaction.token, {
            content: `Posted on [#${number}](${context.repoUrl}/issues/${number}).`,
          });
        },
      };
    }
  }

  return { response: ephemeral("That control is not one this bot knows about.") };
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
        `Comments on the issue appear here; use the button on the issue message to reply.`,
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
