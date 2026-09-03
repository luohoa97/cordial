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
import { container, separator, text, v2 } from "./components.ts";

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

  // Components V2 rather than an embed: a Container gives the accent bar an
  // embed gave, a Separator lets the diagnostics note sit apart from the pitch
  // instead of being another paragraph in one description, and the buttons sit
  // inside the same block rather than floating under it.
  return v2([
    container(0xFF7A18, [
      text(
        "## Report something\n" +
          "Pick the shape that fits and fill in the form. It becomes an issue on " +
          "GitHub and a thread here, and replies travel both ways \u2014 **you do not " +
          "need a GitHub account.**",
      ),
      separator(),
      text(
        "Every form asks for the diagnostics block. Get it from **Settings \u2192 " +
          "Report a Problem \u2192 Copy diagnostics**, or run `cordial --diagnostics` " +
          "\u2014 it works even when the client will not start, which is the report " +
          "that needs it most.",
      ),
      ...rows,
    ]),
  ]);
}
