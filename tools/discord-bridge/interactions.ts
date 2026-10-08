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
import {
  Discord,
  EPHEMERAL,
  InteractionType,
  modalSelections,
  modalValues,
  ResponseType,
} from "./discord.ts";
import {
  ACTION_ROW,
  BUTTON,
  droppedFor,
  type IssueForm,
  LABEL_SELECT_ID,
  labelPickerComponent,
  modalFor,
} from "./issue_forms.ts";
import { container, separator, text } from "./components.ts";
import { GitHub } from "./github.ts";
import {
  fieldSection,
  renderIssueBody,
  renderIssueTitle,
  reporterFromBody,
  type Submission,
} from "./issue_body.ts";
import { type Label, offerLabels, resolveSelection, type Who } from "./labels.ts";
import { isModerator, type Member } from "./permissions.ts";
import { openEditor, submitEdit } from "./edit_flow.ts";

export interface Context {
  forms: () => Promise<IssueForm[]>;
  github: GitHub;
  discord: Discord;
  threadChannelId: string;
  repoUrl: string;
  /**
   * The repository's labels, cached. Absent means the label picker is simply
   * not offered, which is also what a failed fetch looks like -- a report never
   * waits on a classification.
   */
  labels?: { get(timeoutMs?: number): Promise<Label[] | null> };
  /**
   * `DISCORD_LABEL_PICKER=1`. **Off unless set**, because whether Discord accepts
   * a string select inside a modal is unverified, and if it does not, *every*
   * report dialog fails with "interaction failed" and nobody can file anything.
   * Off, the dialogs are text-only and exactly what they were before labels.
   */
  labelPicker?: boolean;
  /** What a reporter may pick; patterns, `*` the only wildcard. See `labels.ts`. */
  reporterLabels?: string[];
  /** Roles that count as moderators in addition to Discord's own permissions. */
  moderatorRoleIds?: string[];
}

interface ResolvedMessage {
  content?: string;
  attachments?: { filename?: string; url?: string; size?: number }[];
  author?: { username?: string; global_name?: string; bot?: boolean };
  id?: string;
}

export interface Interaction {
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
  member?: Member & {
    user?: { id: string; username: string; global_name?: string };
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

/** Who is pressing, with what Discord says about their standing in this channel. */
function editorOf(interaction: Interaction) {
  return { ...reporter(interaction), member: interaction.member };
}

function whoIs(context: Context, interaction: Interaction): Who {
  return isModerator(interaction.member, context.moderatorRoleIds) ? "moderator" : "reporter";
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
  outcome = "nothing was filed",
): () => Promise<void> {
  return async () => {
    try {
      await work();
    } catch (error) {
      const why = error instanceof Error ? error.message : String(error);
      console.error(`follow-up: ${why}`);
      try {
        await context.discord.editOriginal(token, {
          content: `That did not work, and ${outcome}. You can try again — ` +
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
      const picker = await labelPicker(context, form, interaction);
      return { response: { type: ResponseType.MODAL, data: modalFor(form, "main", picker) } };
    }
    if (verb === "cordial-issue-extra") {
      const form = find(rest[0]);
      // `:l` says the main dialog gave a slot to the label picker, which moved
      // one more optional field into this follow-up.
      const withPicker = rest[2] === "l";
      if (!form || !droppedFor(form, withPicker).length) {
        return { response: ephemeral("There is nothing more to add for this form.") };
      }
      const modal = modalFor(form, "extra", withPicker ? true : undefined) as Record<
        string,
        unknown
      >;
      // The issue number rides in the custom_id, because the submit that
      // follows arrives as a fresh interaction with no memory of this one and
      // there is deliberately nowhere to keep it.
      modal.custom_id = `cordial-issue:${form.slug}:extra:${rest[1]}`;
      return { response: { type: ResponseType.MODAL, data: modal } };
    }
    if (verb === "cordial-edit-open") {
      return {
        response: await openEditor(
          context,
          editorOf(interaction),
          Number(rest[0]),
          rest[1] === undefined ? 0 : Number(rest[1]),
        ),
      };
    }
    if (verb === "cordial-close" || verb === "cordial-reopen" || verb === "cordial-fixed") {
      const open = verb === "cordial-reopen";
      const completed = verb === "cordial-fixed";
      if (completed && !isModerator(interaction.member, context.moderatorRoleIds)) {
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
          () =>
            fileIssue(
              context,
              interaction,
              form,
              { values, reporter: who },
              {
                // The `:l` on the dialog's id is the only evidence the picker
                // was in it; see `modalFor`.
                withPicker: rest[2] === "l",
                picks: modalSelections(interaction.data)[LABEL_SELECT_ID] ?? [],
                who: whoIs(context, interaction),
              },
            ),
        ),
      };
    }

    if (verb === "cordial-edit") {
      return {
        response: deferred(),
        after: reporting(
          context,
          interaction.token,
          () =>
            submitEdit(
              context,
              interaction,
              editorOf(interaction),
              {
                number: Number(rest[0]),
                part: Number(rest[1]),
                hash: rest[2] ?? "",
                withLabels: rest[3] === "l",
              },
              { values, selections: modalSelections(interaction.data) },
            ),
          "nothing was changed",
        ),
      };
    }

    if (verb === "cordial-issue" && rest[1] === "extra") {
      const form = find(rest[0]);
      const number = Number(rest[2]);
      if (!form || !Number.isInteger(number)) {
        return { response: ephemeral("That form is no longer available.") };
      }
      // Any field of the form the dialog filled in. The dialog only ever held
      // the fields that did not fit the main one, so naming them again here --
      // and getting it wrong when the label picker has moved one -- buys nothing.
      const extra = form.fields
        .filter((b) => b.id && values[b.id]?.trim())
        .map((b) => fieldSection(b, values[b.id!]))
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
      { type: BUTTON, style: 2, label: "Edit", custom_id: `cordial-edit-open:${number}` },
    ],
  }];
}

/**
 * The label menu for a form's dialog, or nothing.
 *
 * Nothing when there is no label source, when it cannot answer within the time
 * a dialog can wait, or when there is nothing this person may pick -- in every
 * one of which the form is simply the form it always was. The menu is
 * decoration on a report, never a precondition for one.
 */
async function labelPicker(
  context: Context,
  form: IssueForm,
  interaction: Interaction,
): Promise<unknown | undefined> {
  if (!context.labelPicker || !context.labels) return undefined;
  const known = await context.labels.get(1200).catch(() => null);
  if (!known) return undefined;
  const who = whoIs(context, interaction);
  const { offered, omitted } = offerLabels({
    known,
    who,
    allow: context.reporterLabels ?? [],
    formSlug: form.slug,
    // The template applies these itself; offering them would only invite
    // somebody to deselect the one label every bug report is meant to carry.
    exclude: form.labels,
  });
  if (!offered.length) return undefined;
  const note = who === "moderator"
    ? "Optional. Any label."
    : "Optional. Where it happens; maintainers set the rest.";
  return labelPickerComponent(offered, {
    description: omitted ? `${note} ${omitted} more not shown.` : note,
  });
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
  chosen: { withPicker: boolean; picks: string[]; who: Who } = {
    withPicker: false,
    picks: [],
    who: "reporter",
  },
): Promise<void> {
  const title = renderIssueTitle(form, submission);

  // The template's own labels always apply. What the reporter picked is checked
  // here against what they may pick, because the dialog's values are the
  // client's word and the menu was built from a list that may have moved since.
  // **A label problem never stops the report**: it files with what is allowed
  // and says what it left off.
  let labels = [...form.labels];
  let labelNote = "";
  // A dialog opened while the picker was on can still be submitted after it is
  // turned off; its picks are ignored rather than applied behind the flag.
  if (chosen.picks.length && context.labelPicker) {
    const known = context.labels ? await context.labels.get(2000).catch(() => null) : null;
    if (!known) {
      labelNote = "\n\nThe label list could not be read, so no labels were added. A maintainer " +
        "can add them.";
    } else {
      const { applied, rejected } = resolveSelection(
        chosen.picks,
        known,
        chosen.who,
        context.reporterLabels ?? [],
      );
      labels = [...new Set([...labels, ...applied])];
      if (rejected.length) {
        labelNote = `\n\nLeft off, because they cannot be chosen here: ${rejected.join(", ")}.`;
      }
      if (applied.length) labelNote = `\n\nLabels: ${applied.join(", ")}.` + labelNote;
    }
  }

  const issue = await context.github.createIssue(
    title,
    renderIssueBody(form, submission, null),
    labels,
  );

  let threadId: string | null = null;
  try {
    threadId = await context.discord.openThread(
      context.threadChannelId,
      `#${issue.number} ${title}`.slice(0, 100),
      [
        container(0xFF7A18, [
          text(`### ${title}\n${issue.html_url}`),
          separator(),
          text(
            `Filed by ${submission.reporter.tag}. Comments on the issue appear here. ` +
              `To add something from this thread, right-click a message → Apps → ` +
              `**Add to the issue**.`,
          ),
          ...threadControls(issue.number),
        ]),
      ],
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

  const leftover = droppedFor(form, chosen.withPicker);
  const more = leftover.length
    ? [{
      type: ACTION_ROW,
      components: [{
        type: BUTTON,
        style: 2,
        label: "Add the rest",
        custom_id: `cordial-issue-extra:${form.slug}:${issue.number}${
          chosen.withPicker ? ":l" : ""
        }`,
      }],
    }]
    : [];

  await context.discord.editOriginal(interaction.token, {
    content: `Filed as [#${issue.number}](${issue.html_url})` +
      (threadId ? ` — follow it in <#${threadId}>.` : ", but the thread could not be opened.") +
      (leftover.length
        ? `\n\nThis form has ${leftover.length} more optional field(s) that did not ` +
          `fit in one dialog. They help, and you can skip them.`
        : "") +
      labelNote,
    components: more,
  });
}

/**
 * Links to a message's attachments, as a Markdown list, or "" if it has none.
 *
 * **Links, not copies, and the difference is the limit of what the bridge can
 * do.** GitHub's REST API has no way to attach a file to an issue -- the
 * drag-and-drop on the web form uses an endpoint that needs a browser session --
 * so a log archive cannot be uploaded to the issue from here. A Discord link
 * works for a maintainer who reads it soon and is signed in to the server;
 * Discord signs these URLs and they stop working after a while, which the
 * comment says so nobody is surprised by it. The reporter can always attach the
 * file themselves from GitHub if they make an account.
 */
export function attachmentLines(
  attachments: { filename?: string; url?: string; size?: number }[] | undefined,
): string {
  const lines = (attachments ?? [])
    .filter((a) => a.url && /^https:\/\//.test(a.url))
    .map((a) => {
      const name = (a.filename ?? "attachment").replace(/[\[\]()`]/g, "_");
      const size = a.size ? ` (${Math.ceil(a.size / 1024)} KiB)` : "";
      return `- [${name}](${a.url})${size}`;
    });
  if (!lines.length) return "";
  return `Attached in Discord:\n\n${lines.join("\n")}\n\n` +
    `_Discord links expire after a while; if one has stopped working, ask the reporter to send the file again._`;
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
  const files = attachmentLines(message?.attachments);
  if (!text && !files) {
    return await say(
      `Nothing to add: that message has no text the bot can read. If it was an ` +
        `embed, quote the part that matters in a reply and add that instead.`,
    );
  }

  const author = message?.author?.global_name ?? message?.author?.username ?? "someone";
  const link = interaction.guild_id && target
    ? `\n\n<https://discord.com/channels/${interaction.guild_id}/${channel}/${target}>`
    : "";
  await context.github.comment(
    number,
    `**${author}** in Discord:\n\n${[text, files].filter(Boolean).join("\n\n")}${link}`,
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
