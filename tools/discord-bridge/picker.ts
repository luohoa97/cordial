/**
 * The pinned message that offers the forms.
 *
 * It leads with the thing that stops people filing: **you do not need a GitHub
 * account.** The rest of the copy exists to make the first field answerable
 * before the modal opens, because a required Diagnostics box is where somebody
 * abandons a report if they meet it cold with no idea where to get one.
 *
 * A link to the web forms stays on the message. Somebody who has an account
 * and would rather use it should not be routed through a bot, and the tracker
 * is the same either way.
 */
import { ACTION_ROW, BUTTON, type IssueForm } from "./issue_forms.ts";

/** Discord allows five buttons to a row. */
const PER_ROW = 5;

export function pickerMessage(forms: IssueForm[], repoUrl: string): unknown {
  const buttons = forms.map((form) => ({
    type: BUTTON,
    style: 2,
    label: form.name,
    custom_id: `cordial-issue-open:${form.slug}`,
  }));

  const rows: unknown[] = [];
  for (let i = 0; i < buttons.length; i += PER_ROW) {
    rows.push({ type: ACTION_ROW, components: buttons.slice(i, i + PER_ROW) });
  }
  rows.push({
    type: ACTION_ROW,
    components: [{
      type: BUTTON,
      style: 5,
      label: "Open on GitHub instead",
      url: `${repoUrl}/issues/new/choose`,
    }],
  });

  return {
    embeds: [{
      title: "Report something",
      description: "Pick the shape that fits and fill in the form. It becomes an issue " +
        "on GitHub and a thread here, and replies travel both ways — **you do " +
        "not need a GitHub account.**\n\n" +
        "Every form asks for the diagnostics block. Get it from **Settings → " +
        "Report a Problem → Copy diagnostics**, or run `cordial --diagnostics` " +
        "— it works even when the client will not start, which is the report " +
        "that needs it most.",
      color: 0xFF7A18,
    }],
    components: rows,
  };
}
