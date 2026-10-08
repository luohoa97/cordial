import { TEMPLATE_DIR } from "./repo.ts";
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@^1.0.8";
import { InteractionType, ResponseType } from "./discord.ts";
import { type Context, handle } from "./interactions.ts";
import { modalFor, parseForm } from "./issue_forms.ts";
import { threadFromBody } from "./issue_body.ts";

const forms = [...Deno.readDirSync(TEMPLATE_DIR)]
  .filter((e) => e.isFile && e.name.endsWith(".yml") && e.name !== "config.yml")
  .map((e) =>
    parseForm(e.name.replace(/\.yml$/, ""), Deno.readTextFileSync(`${TEMPLATE_DIR}/${e.name}`))
  );

/** Records what would have been sent, so the whole flow runs with no network. */
function fakes() {
  const calls: { what: string; args: unknown[] }[] = [];
  const record = (what: string) => (...args: unknown[]) => {
    calls.push({ what, args });
    return Promise.resolve();
  };
  const context = {
    forms: () => Promise.resolve(forms),
    github: {
      createIssue: (...args: unknown[]) => {
        calls.push({ what: "createIssue", args });
        return Promise.resolve({ number: 12, html_url: "https://github.com/o/r/issues/12" });
      },
      setIssueBody: record("setIssueBody"),
      comment: record("comment"),
    },
    discord: {
      openThread: (...args: unknown[]) => {
        calls.push({ what: "openThread", args });
        return Promise.resolve("777");
      },
      post: record("post"),
      editOriginal: record("editOriginal"),
    },
    threadChannelId: "chan",
    repoUrl: "https://github.com/o/r",
  } as unknown as Context;
  return { context, calls, of: (what: string) => calls.filter((c) => c.what === what) };
}

const user = { member: { user: { id: "9", username: "someone", global_name: "Someone" } } };

Deno.test("a ping is answered with a pong, which is how Discord registers the endpoint", async () => {
  const { context } = fakes();
  const { response } = await handle(context, { type: InteractionType.PING, token: "t" });
  assertEquals(response, { type: ResponseType.PONG });
});

Deno.test("pressing a form's button opens that form's modal, and does not defer", async () => {
  // Deferring here would lose the modal entirely: it is only valid as an
  // interaction's *first* response.
  const { context } = fakes();
  const { response, after } = await handle(context, {
    type: InteractionType.MESSAGE_COMPONENT,
    token: "t",
    data: { custom_id: "cordial-issue-open:bug_report" },
    ...user,
  });
  const body = response as { type: number; data: { custom_id: string } };
  assertEquals(body.type, ResponseType.MODAL);
  assertEquals(body.data.custom_id, "cordial-issue:bug_report:main");
  assertEquals(after, undefined);
});

Deno.test("submitting the form defers, then files the issue and pairs the thread", async () => {
  const { context, of } = fakes();
  const { response, after } = await handle(context, {
    type: InteractionType.MODAL_SUBMIT,
    token: "tok",
    data: {
      custom_id: "cordial-issue:bug_report:main",
      components: [
        { type: 18, component: { custom_id: "what-happened", value: "Black window." } },
        { type: 18, component: { custom_id: "diagnostics", value: "Cordial 0.13.2" } },
      ],
    },
    ...user,
  } as never);

  // Deferred immediately: filing is three round trips and Discord allows three
  // seconds.
  assertEquals((response as { type: number }).type, ResponseType.DEFERRED_MESSAGE);
  assert(after);
  await after();

  const [created] = of("createIssue");
  assertEquals(created.args[0], "[Bug]: Black window.");
  assertStringIncludes(created.args[1] as string, "### What happened");
  assertStringIncludes(created.args[1] as string, "Someone");
  assertEquals(created.args[2], ["bug"]);

  // The pairing is written back only after the thread exists, which is the
  // whole reason for the second write.
  const [thread] = of("openThread");
  assertStringIncludes(thread.args[1] as string, "#12");
  assertEquals(threadFromBody(of("setIssueBody")[0].args[1] as string), "777");

  const [reply] = of("editOriginal");
  assertStringIncludes((reply.args[1] as { content: string }).content, "#12");
  assertStringIncludes((reply.args[1] as { content: string }).content, "777");
});

Deno.test("a form with leftovers offers them, and a form without does not", async () => {
  for (const [slug, expected] of [["bug_report", true], ["feature", false]] as const) {
    const { context, of } = fakes();
    const { after } = await handle(context, {
      type: InteractionType.MODAL_SUBMIT,
      token: "tok",
      data: {
        custom_id: `cordial-issue:${slug}:main`,
        components: [{ type: 18, component: { custom_id: "diagnostics", value: "d" } }],
      },
      ...user,
    } as never);
    await after!();
    const reply = of("editOriginal")[0].args[1] as { components?: unknown[] };
    assertEquals(
      Boolean(reply.components?.length),
      expected,
      `${slug} should ${expected ? "" : "not "}offer a follow-up`,
    );
  }
});

Deno.test("a select's chosen option is read out of the submission, not only text", async () => {
  const { context, of } = fakes();
  const { after } = await handle(context, {
    type: InteractionType.MODAL_SUBMIT,
    token: "tok",
    data: {
      custom_id: "cordial-issue:finding:main",
      components: [
        { type: 18, component: { custom_id: "what-you-established", value: "It parks." } },
        { type: 18, component: { custom_id: "confidence", values: ["Measured, with a control"] } },
      ],
    },
    ...user,
  } as never);
  await after!();
  assertStringIncludes(of("createIssue")[0].args[1] as string, "Measured, with a control");
});

Deno.test("a comment from the thread is posted with the Discord name against it", async () => {
  const { context, of } = fakes();
  const { after } = await handle(context, {
    type: InteractionType.MODAL_SUBMIT,
    token: "tok",
    data: {
      custom_id: "cordial-comment:12",
      components: [{ type: 18, component: { custom_id: "comment", value: "Still broken." } }],
    },
    ...user,
  } as never);
  await after!();
  const [posted] = of("comment");
  assertEquals(posted.args[0], 12);
  assertStringIncludes(posted.args[1] as string, "Someone");
  assertStringIncludes(posted.args[1] as string, "Still broken.");
});

Deno.test("a button for a form that no longer exists says so instead of failing", async () => {
  // Old pinned messages outlive template renames, and the person pressing the
  // button has done nothing wrong.
  const { context } = fakes();
  const { response } = await handle(context, {
    type: InteractionType.MESSAGE_COMPONENT,
    token: "t",
    data: { custom_id: "cordial-issue-open:no_such_form" },
    ...user,
  });
  const body = response as { type: number; data: { content: string; flags: number } };
  assertEquals(body.type, ResponseType.MESSAGE);
  assertStringIncludes(body.data.content, "no_such_form");
  assertEquals(body.data.flags, 1 << 6, "and only the presser sees it");
});

const messageCommand = (overrides: Record<string, unknown> = {}) => ({
  type: 2,
  token: "tok",
  channel_id: "thread-1",
  guild_id: "g1",
  data: {
    name: "Add to the issue",
    type: 3,
    target_id: "m1",
    resolved: {
      messages: {
        m1: { content: "It also happens on X11.", author: { global_name: "Reporter" } },
      },
    },
  },
  ...overrides,
} as never);

Deno.test("the context menu adds one chosen message to the thread's issue", async () => {
  const { context, of } = fakes();
  (context.discord as unknown as { channelName: () => Promise<string> }).channelName = () =>
    Promise.resolve("#24 [Bug]: something");

  const { response, after } = await handle(context, messageCommand());
  assertEquals((response as { type: number }).type, ResponseType.DEFERRED_MESSAGE);
  await after!();

  const [posted] = of("comment");
  assertEquals(posted.args[0], 24, "the issue number comes from the thread name");
  assertStringIncludes(posted.args[1] as string, "Reporter");
  assertStringIncludes(posted.args[1] as string, "It also happens on X11.");
  // A link back, so a maintainer can see the conversation it came from.
  assertStringIncludes(posted.args[1] as string, "discord.com/channels/g1/thread-1/m1");
});

Deno.test("used outside an issue thread it explains itself and posts nothing", async () => {
  const { context, of } = fakes();
  (context.discord as unknown as { channelName: () => Promise<string> }).channelName = () =>
    Promise.resolve("general");

  const { after } = await handle(context, messageCommand());
  await after!();
  assertEquals(of("comment").length, 0, "nothing may reach the tracker");
  assertStringIncludes(
    (of("editOriginal")[0].args[1] as { content: string }).content,
    "#<number>",
  );
});

Deno.test("a message with no readable text is refused rather than posted blank", async () => {
  // An attachment-only message has no content, and Discord does not document
  // whether a bot without the Message Content intent sees content here at all
  // -- so an empty body must never become an empty comment.
  const { context, of } = fakes();
  (context.discord as unknown as { channelName: () => Promise<string> }).channelName = () =>
    Promise.resolve("#7 [Bug]: x");

  const { after } = await handle(
    context,
    messageCommand({
      data: {
        name: "Add to the issue",
        type: 3,
        target_id: "m1",
        resolved: { messages: { m1: { content: "   ", author: { username: "a" } } } },
      },
    }),
  );
  await after!();
  assertEquals(of("comment").length, 0);
  assertStringIncludes(
    (of("editOriginal")[0].args[1] as { content: string }).content,
    "no text the bot can read",
  );
});

Deno.test("a follow-up that throws tells the reporter instead of hanging", async () => {
  // The "is thinking" spinner never stops on its own. Every deferred path must
  // land a reply even when the work fails.
  const { context, of } = fakes();
  (context.github as unknown as { createIssue: () => Promise<never> }).createIssue = () =>
    Promise.reject(new Error("GitHub is having a day"));

  const { after } = await handle(context, {
    type: InteractionType.MODAL_SUBMIT,
    token: "tok",
    data: {
      custom_id: "cordial-issue:bug_report:main",
      components: [{ type: 18, component: { custom_id: "diagnostics", value: "d" } }],
    },
    ...user,
  } as never);
  await after!();

  const [reply] = of("editOriginal");
  assert(reply, "a failed follow-up must still edit the deferred reply");
  const content = (reply.args[1] as { content: string }).content;
  assertStringIncludes(content, "nothing was filed");
  assertStringIncludes(content, "GitHub is having a day");
});

/** An issue as GitHub returns it, paired to a Discord reporter. */
function issueOwnedBy(reporterId: string | null, state = "open") {
  return {
    state,
    title: "[Bug]: x",
    body: reporterId
      ? `text\n\n<!-- cordial-bridge thread=777 reporter=${reporterId} -->`
      : "filed on the web, no marker",
  };
}

function withIssue(issue: unknown) {
  const f = fakes();
  (f.context.github as unknown as { issue: () => Promise<unknown> }).issue = () =>
    Promise.resolve(issue);
  (f.context.github as unknown as { setIssueOpen: (...a: unknown[]) => Promise<void> })
    .setIssueOpen = (...args: unknown[]) => {
      f.calls.push({ what: "setIssueOpen", args });
      return Promise.resolve();
    };
  (f.context.discord as unknown as { setArchived: (...a: unknown[]) => Promise<void> })
    .setArchived = (...args: unknown[]) => {
      f.calls.push({ what: "setArchived", args });
      return Promise.resolve();
    };
  return f;
}

const press = (customId: string, extra: Record<string, unknown> = {}) => ({
  type: InteractionType.MESSAGE_COMPONENT,
  token: "tok",
  channel_id: "777",
  data: { custom_id: customId },
  member: { user: { id: "9", username: "someone", global_name: "Someone" } },
  ...extra,
} as never);

Deno.test("the reporter can close their own issue, and the thread follows it", async () => {
  const f = withIssue(issueOwnedBy("9"));
  const { after } = await handle(f.context, press("cordial-close:31"));
  await after!();

  const [closed] = f.of("setIssueOpen");
  assertEquals(closed.args, [31, false, false], "closed, and not as completed");
  assertEquals(f.of("setArchived")[0].args, ["777", true], "the thread is archived, not locked");
  assertStringIncludes(f.of("comment")[0].args[1] as string, "Someone");
});

Deno.test("somebody else pressing close changes nothing", async () => {
  // The custom_id is client-supplied and anyone who can see the message can
  // press it, so the check has to be against the issue, not the button.
  const f = withIssue(issueOwnedBy("1234567890"));
  const { after } = await handle(f.context, press("cordial-close:31"));
  await after!();

  assertEquals(f.of("setIssueOpen").length, 0, "the issue must not move");
  assertEquals(f.of("setArchived").length, 0);
  assertStringIncludes(
    (f.of("editOriginal")[0].args[1] as { content: string }).content,
    "Only the person who filed this",
  );
});

Deno.test("an issue filed on the web cannot be closed from Discord at all", async () => {
  const f = withIssue(issueOwnedBy(null));
  const { after } = await handle(f.context, press("cordial-close:31"));
  await after!();
  assertEquals(f.of("setIssueOpen").length, 0);
  assertStringIncludes(
    (f.of("editOriginal")[0].args[1] as { content: string }).content,
    "not filed from Discord",
  );
});

Deno.test("marking completed needs a permission the reporter does not have", async () => {
  // "This is fixed" is a claim about the project; "I do not need this" is the
  // reporter's own. They must not be the same button or the same permission.
  const f = withIssue(issueOwnedBy("9"));
  const { response, after } = await handle(f.context, press("cordial-fixed:31"));
  assertEquals(after, undefined, "it must not even defer");
  const body = response as { type: number; data: { content: string; flags: number } };
  assertEquals(body.data.flags, 1 << 6);
  assertStringIncludes(body.data.content, "helps run this server");
});

Deno.test("a maintainer marks it completed, and that is a different close", async () => {
  const f = withIssue(issueOwnedBy("1234567890"));
  const { after } = await handle(
    f.context,
    press("cordial-fixed:31", {
      // Manage Messages. Not the reporter -- deliberately, because a
      // maintainer may close an issue they did not file.
      member: { user: { id: "maint", username: "m" }, permissions: String(1n << 13n) },
    }),
  );
  await after!();
  assertEquals(f.of("setIssueOpen")[0].args, [31, false, true], "closed as completed");
});

Deno.test("reopening puts the issue and the thread back", async () => {
  const f = withIssue(issueOwnedBy("9", "closed"));
  const { after } = await handle(f.context, press("cordial-reopen:31"));
  await after!();
  assertEquals(f.of("setIssueOpen")[0].args, [31, true, false]);
  assertEquals(f.of("setArchived")[0].args, ["777", false], "unarchived before posting into it");
});

Deno.test("pressing close on an already-closed issue says so and does nothing", async () => {
  const f = withIssue(issueOwnedBy("9", "closed"));
  const { after } = await handle(f.context, press("cordial-close:31"));
  await after!();
  assertEquals(f.of("setIssueOpen").length, 0);
  assertStringIncludes(
    (f.of("editOriginal")[0].args[1] as { content: string }).content,
    "already closed",
  );
});

Deno.test("the thread's first message carries every control", async () => {
  const f = fakes();
  const { after } = await handle(f.context, {
    type: InteractionType.MODAL_SUBMIT,
    token: "tok",
    data: {
      custom_id: "cordial-issue:bug_report:main",
      components: [{ type: 18, component: { custom_id: "diagnostics", value: "d" } }],
    },
    ...user,
  } as never);
  await after!();

  // The opening post is Components V2: one Container holding the text and the
  // controls, so the buttons are in the first message rather than below it.
  const opening = f.of("openThread")[0].args[2] as { type: number; components: unknown[] }[];
  assertEquals(opening[0].type, 17, "a Container");
  const rows = opening[0].components.filter((c) => (c as { type: number }).type === 1);
  assertEquals(rows.length, 1, "one action row, inside the container");
  const ids = (rows[0] as { components: { custom_id: string }[] }).components
    .map((c) => c.custom_id);
  assertEquals(ids, [
    "cordial-comment:12",
    "cordial-fixed:12",
    "cordial-close:12",
    "cordial-reopen:12",
    "cordial-edit-open:12",
  ]);
});

// ---------------------------------------------------------------------------
// Labels in the report form, and editing a filed report.
// ---------------------------------------------------------------------------

import { renderIssueBody } from "./issue_body.ts";
import type { Label } from "./labels.ts";

const bugForm = forms.find((f) => f.slug === "bug_report")!;
const repoLabels: Label[] = [
  "bug",
  "enhancement",
  "confirmed",
  "wontfix",
  "priority: high",
  "area:graphics",
  "area:input",
  "gpu:nvidia",
  "compositor:sway",
].map((name) => ({ name }));
const ALLOW = ["area:*", "platform:*", "compositor:*", "gpu:*"];

type Json = Record<string, unknown>;

/** The fakes, plus a GitHub that remembers the issue and what was done to it. */
function tracker(
  options: {
    filer?: string | null;
    known?: Label[] | null;
    labelsOnIssue?: string[];
    roles?: string[];
    body?: string;
    picker?: boolean;
  } = {},
) {
  const f = fakes();
  const values = {
    "what-happened": "The window opens and stays black.",
    "what-expected": "The Roblox home screen.",
    "repro": "Launch it.",
    "diagnostics": "Cordial 0.13.2 (91f8ee9)\nInstall rpm",
  };
  const filer = options.filer === undefined ? "9" : options.filer;
  const issue = {
    title: "[Bug]: The window opens and stays black.",
    state: "open",
    labels: options.labelsOnIssue ?? ["bug"],
    body: options.body ??
      (filer
        ? renderIssueBody(bugForm, { values, reporter: { id: filer, tag: "Someone" } }, "777")
        : "### What happened\n\nfiled on the web"),
  };
  const github = f.context.github as unknown as Json;
  github.issue = () => Promise.resolve({ ...issue, labels: [...issue.labels] });
  github.comment = (...args: unknown[]) => {
    f.calls.push({ what: "comment", args });
    return Promise.resolve({
      id: 55,
      html_url: "https://github.com/o/r/issues/12#issuecomment-55",
    });
  };
  github.updateIssue = (number: number, fields: Json) => {
    f.calls.push({ what: "updateIssue", args: [number, fields] });
    if (typeof fields.title === "string") issue.title = fields.title;
    if (typeof fields.body === "string") issue.body = fields.body;
    if (Array.isArray(fields.labels)) issue.labels = fields.labels as string[];
    return Promise.resolve();
  };
  github.editComment = f.calls.push.bind(f.calls) && ((...args: unknown[]) => {
    f.calls.push({ what: "editComment", args });
    return Promise.resolve();
  });
  const discord = f.context.discord as unknown as Json;
  discord.renameThread = (...args: unknown[]) => {
    f.calls.push({ what: "renameThread", args });
    return Promise.resolve();
  };
  const known = options.known === undefined ? repoLabels : options.known;
  f.context.labels = { get: () => Promise.resolve(known) };
  f.context.labelPicker = options.picker ?? true;
  f.context.reporterLabels = ALLOW;
  f.context.moderatorRoleIds = options.roles ?? [];
  return { ...f, issue };
}

const asUser = (id: string, extra: Json = {}) => ({
  member: { user: { id, username: `u${id}`, global_name: `User ${id}` }, ...extra },
});
const MOD = asUser("mod", { permissions: String(1n << 13n) });

const pressAs = (customId: string, who: Json = asUser("9")) => ({
  type: InteractionType.MESSAGE_COMPONENT,
  token: "tok",
  channel_id: "777",
  data: { custom_id: customId },
  ...who,
} as never);

/** What a person types into a modal, in the shape Discord submits it. */
function submission(
  modal: { custom_id: string },
  values: Record<string, string>,
  selections: Record<string, string[]> = {},
  who: Json = asUser("9"),
) {
  return {
    type: InteractionType.MODAL_SUBMIT,
    token: "tok",
    channel_id: "777",
    data: {
      custom_id: modal.custom_id,
      components: [
        ...Object.entries(values).map(([custom_id, value]) => ({
          type: 18,
          component: { custom_id, value },
        })),
        ...Object.entries(selections).map(([custom_id, v]) => ({
          type: 18,
          component: { custom_id, values: v },
        })),
      ],
    },
    ...who,
  } as never;
}

type Modal = { custom_id: string; title: string; components: Json[] };
const modalOf = (response: unknown) => (response as { data: Modal }).data;
const inputs = (modal: Modal) =>
  modal.components.map((c) => (c.component as Json).custom_id as string);
const said = (calls: { what: string; args: unknown[] }[]) =>
  (calls.filter((c) => c.what === "editOriginal").at(-1)!.args[1] as { content: string }).content;

Deno.test("the report form offers a reporter the allowed labels, in the fifth slot", async () => {
  const t = tracker();
  const { response } = await handle(t.context, press("cordial-issue-open:bug_report"));
  const modal = modalOf(response);
  assertEquals((response as { type: number }).type, ResponseType.MODAL);
  assert(modal.components.length <= 5, "a modal never holds more than five");
  // Every required field kept its place; the picker took the optional one's.
  const ids = inputs(modal);
  for (const required of ["what-happened", "what-expected", "repro", "diagnostics"]) {
    assert(ids.includes(required), `${required} must survive the picker`);
  }
  assertEquals(ids.at(-1), "cordial-labels");
  assert(modal.custom_id.endsWith(":main:l"), modal.custom_id);

  const picker = modal.components.at(-1)!.component as { options: { value: string }[] };
  const offered = picker.options.map((o) => o.value);
  assertEquals(offered, ["gpu:nvidia", "compositor:sway", "area:graphics", "area:input"]);
  for (const hidden of ["confirmed", "wontfix", "priority: high", "bug"]) {
    assert(!offered.includes(hidden), `${hidden} must not be offered to a reporter`);
  }
});

Deno.test("a moderator's form offers every label except the ones the template applies", async () => {
  const t = tracker();
  const { response } = await handle(t.context, press("cordial-issue-open:bug_report", MOD));
  const picker = modalOf(response).components.at(-1)!.component as {
    options: { value: string }[];
  };
  const offered = picker.options.map((o) => o.value);
  assert(offered.includes("confirmed") && offered.includes("priority: high"));
  assert(!offered.includes("bug"), "bug is applied by the template already");
});

Deno.test("with no label the reporter may pick, the form is exactly what it was", async () => {
  const t = tracker({ known: L("bug", "confirmed") });
  const { response } = await handle(t.context, press("cordial-issue-open:bug_report"));
  const modal = modalOf(response);
  assert(!inputs(modal).includes("cordial-labels"));
  assertEquals(modal.custom_id, "cordial-issue:bug_report:main");
  assertEquals(modal.components.length, 5);
});

Deno.test("if the label list cannot be had, the form opens without a picker", async () => {
  const t = tracker({ known: null });
  const { response } = await handle(t.context, press("cordial-issue-open:bug_report"));
  assert(!inputs(modalOf(response)).includes("cordial-labels"));
  assertEquals(modalOf(response).custom_id, "cordial-issue:bug_report:main");
});

function L(...names: string[]): Label[] {
  return names.map((name) => ({ name }));
}

const fileWithPicks = (picks: string[], custom = "cordial-issue:bug_report:main:l") =>
  submission(
    { custom_id: custom },
    {
      "what-happened": "Black window.",
      "what-expected": "Home.",
      "repro": "Launch.",
      "diagnostics": "Cordial 0.13.2",
    },
    { "cordial-labels": picks },
  );

Deno.test("the chosen labels are applied when the issue is created", async () => {
  const t = tracker();
  const { after } = await handle(t.context, fileWithPicks(["gpu:nvidia", "area:input"]));
  await after!();
  const [created] = t.of("createIssue");
  assertEquals(created.args[2], ["bug", "gpu:nvidia", "area:input"], "template label first");
  assertStringIncludes(said(t.calls), "Labels: gpu:nvidia, area:input");
});

Deno.test("a reporter who forges a protected label gets the report without it", async () => {
  const t = tracker();
  const { after } = await handle(
    t.context,
    fileWithPicks(["confirmed", "priority: high", "wontfix", "area:input", "not-a-label"]),
  );
  await after!();
  assertEquals(t.of("createIssue")[0].args[2], ["bug", "area:input"]);
  const reply = said(t.calls);
  assertStringIncludes(reply, "cannot be chosen here");
  assertStringIncludes(reply, "confirmed");
  assertStringIncludes(reply, "Filed as", "the report still filed");
});

Deno.test("a moderator filing a report may apply any label", async () => {
  const t = tracker();
  const { after } = await handle(
    t.context,
    { ...(fileWithPicks(["confirmed", "priority: high"]) as Json), ...MOD } as never,
  );
  await after!();
  assertEquals(t.of("createIssue")[0].args[2], ["bug", "confirmed", "priority: high"]);
});

Deno.test("if the label list is gone at submit, the report files without labels and says so", async () => {
  const t = tracker({ known: null });
  const { after } = await handle(t.context, fileWithPicks(["area:input"]));
  await after!();
  assertEquals(t.of("createIssue")[0].args[2], ["bug"]);
  assertStringIncludes(said(t.calls), "no labels were added");
  assertStringIncludes(said(t.calls), "Filed as");
});

Deno.test("the optional field the picker displaced is offered in the follow-up", async () => {
  const t = tracker();
  const { after } = await handle(t.context, fileWithPicks([]));
  await after!();
  const reply = t.of("editOriginal")[0].args[1] as {
    content: string;
    components: { components: { custom_id: string }[] }[];
  };
  const button = reply.components[0].components[0].custom_id;
  assertEquals(button, "cordial-issue-extra:bug_report:12:l");

  const { response } = await handle(t.context, press(button));
  const extra = modalOf(response);
  assert(extra.components.length <= 5);
  // Five optional fields in all now, one more than without the picker.
  assertEquals(extra.components.length, 5);
  assertEquals(inputs(extra)[0], "engine-log", "the field that gave way is first in line");
});

Deno.test("a report filed with no picks needs no label lookup at all", async () => {
  const t = tracker({ known: null });
  const { after } = await handle(t.context, fileWithPicks([]));
  await after!();
  assertEquals(t.of("createIssue")[0].args[2], ["bug"]);
  assert(!said(t.calls).includes("label"), "nothing to apologise for");
});

Deno.test("Mark as completed also accepts a configured moderator role", async () => {
  const t = tracker({ roles: ["555"] });
  const withIssue = t.context.github as unknown as Json;
  withIssue.setIssueOpen = (...a: unknown[]) => {
    t.calls.push({ what: "setIssueOpen", args: a });
    return Promise.resolve();
  };
  (t.context.discord as unknown as Json).setArchived = () => Promise.resolve();
  const { after } = await handle(
    t.context,
    pressAs("cordial-fixed:12", asUser("r", { roles: ["555"] })),
  );
  await after!();
  assertEquals(t.of("setIssueOpen")[0].args, [12, false, true]);
});

// ---- Edit ---------------------------------------------------------------

const openEditor = async (t: ReturnType<typeof tracker>, who: Json = asUser("9"), id = "12") => {
  const { response, after } = await handle(t.context, pressAs(`cordial-edit-open:${id}`, who));
  return { response, after };
};

Deno.test("the original poster gets a dialog pre-filled with the report", async () => {
  const t = tracker();
  const { response, after } = await openEditor(t);
  assertEquals((response as { type: number }).type, ResponseType.MODAL);
  assertEquals(after, undefined, "a modal cannot follow a deferral");
  const modal = modalOf(response);
  assertEquals(modal.title, "Edit #12 (1 of 2)");
  assert(modal.components.length <= 5);
  assertEquals(inputs(modal), [
    "cordial-title",
    "what-happened",
    "what-expected",
    "repro",
    "cordial-labels",
  ]);
  const title = modal.components[0].component as Json;
  assertEquals(title.value, "[Bug]: The window opens and stays black.");
  const first = modal.components[1].component as Json;
  assertEquals(first.value, "The window opens and stays black.");
  assert(!inputs(modal).includes("diagnostics"), "the diagnostics block is not editable");
  // What is on the issue is shown as chosen -- but `bug` is not the reporter's.
  const options = (modal.components[4].component as { options: { value: string }[] }).options;
  assert(!options.some((o) => o.value === "bug"));
});

Deno.test("a moderator can open the editor on somebody else's report, by permission or by role", async () => {
  for (const [who, roles] of [[MOD, []], [asUser("r", { roles: ["555"] }), ["555"]]] as const) {
    const t = tracker({ roles: [...roles] });
    const { response } = await openEditor(t, who as Json);
    assertEquals((response as { type: number }).type, ResponseType.MODAL);
  }
});

Deno.test("anybody else is refused, ephemerally, and nothing is fetched beyond the issue", async () => {
  const t = tracker();
  const { response, after } = await openEditor(t, asUser("1234"));
  const body = response as { type: number; data: { content: string; flags: number } };
  assertEquals(body.type, ResponseType.MESSAGE);
  assertEquals(body.data.flags, 1 << 6, "only the presser sees it");
  assertStringIncludes(body.data.content, "Only the person who filed this report");
  assertEquals(after, undefined);
});

Deno.test("an issue with no recorded filer can be edited by a moderator and nobody else", async () => {
  const stranger = tracker({ filer: null });
  assertEquals(
    ((await openEditor(stranger, asUser("9"))).response as { type: number }).type,
    ResponseType.MESSAGE,
  );
  const mod = tracker({ filer: null });
  const { response } = await openEditor(mod, MOD);
  assertEquals((response as { type: number }).type, ResponseType.MODAL);
});

Deno.test("a field the issue never had is not made mandatory by editing it", async () => {
  // Filed on the web with only one answer. The template's other required
  // fields are absent, and a moderator fixing a typo must not be forced to
  // invent them.
  const t = tracker({ filer: null });
  const { response } = await openEditor(t, MOD);
  const modal = modalOf(response);
  const byId = Object.fromEntries(
    modal.components.map((c) => [(c.component as Json).custom_id, c.component as Json]),
  );
  assertEquals(byId["what-happened"].required, true, "it is there, so it stays required");
  assertEquals(byId["what-expected"].required, false);
  assertEquals(byId["repro"].required, false);
});

Deno.test("text the bridge cannot make sense of offers title and labels only", async () => {
  const t = tracker({
    filer: "9",
    body: "just some words, no headings\n\n<!-- cordial-bridge thread=777 reporter=9 -->",
  });
  const { response } = await openEditor(t);
  assertEquals(inputs(modalOf(response)), ["cordial-title", "cordial-labels"]);
});

Deno.test("the dialog opens without labels if the label list is not there in time", async () => {
  const t = tracker({ known: null });
  const { response } = await openEditor(t);
  assert(!inputs(modalOf(response)).includes("cordial-labels"));
  assert(!modalOf(response).custom_id.endsWith(":l"));
});

async function openAndSubmit(
  t: ReturnType<typeof tracker>,
  values: Record<string, string>,
  selections?: Record<string, string[]>,
  who: Json = asUser("9"),
  part = 0,
) {
  const { response } = await handle(
    t.context,
    pressAs(`cordial-edit-open:12${part ? `:${part}` : ""}`, who),
  );
  const modal = modalOf(response);
  const { response: ack, after } = await handle(
    t.context,
    submission(modal, values, selections, who),
  );
  assertEquals((ack as { type: number }).type, ResponseType.DEFERRED_MESSAGE);
  await after!();
  return modal;
}

Deno.test("an edit is logged on the issue first, then applied, then logged in the thread", async () => {
  const t = tracker();
  await openAndSubmit(
    t,
    {
      "cordial-title": "[Bug]: Black window on Sway",
      "what-happened": "The window is black on Sway 1.10.",
      "what-expected": "The Roblox home screen.",
      "repro": "Launch it.",
    },
    { "cordial-labels": [] },
  );

  const order = t.calls.map((c) => c.what).filter((w) =>
    ["comment", "updateIssue", "post", "renameThread"].includes(w)
  );
  assertEquals(order, ["comment", "updateIssue", "post", "renameThread"]);

  const audit = t.of("comment")[0].args[1] as string;
  assertStringIncludes(audit, "**Edited from Discord** by `User 9` (`9`, the reporter)");
  assertStringIncludes(
    audit,
    "`[Bug]: The window opens and stays black.` → `[Bug]: Black window on Sway`",
  );
  assertStringIncludes(audit, "<details>");
  assertStringIncludes(audit, "The window opens and stays black.");

  const [update] = t.of("updateIssue");
  const fields = update.args[1] as { title: string; body: string; labels?: string[] };
  assertEquals(fields.title, "[Bug]: Black window on Sway");
  assertStringIncludes(fields.body, "The window is black on Sway 1.10.");
  // The reporter's other words, the diagnostics, the credit and the pairing are intact.
  assertStringIncludes(fields.body, "The Roblox home screen.");
  assertStringIncludes(fields.body, "Cordial 0.13.2 (91f8ee9)\nInstall rpm");
  assertStringIncludes(fields.body, "Filed by **Someone** (`9`)");
  assertStringIncludes(
    fields.body,
    "<!-- cordial-bridge thread=777 reporter=9 form=bug_report -->",
  );
  assertEquals(fields.labels, undefined, "labels were not touched, so they are not sent");

  const [thread] = t.of("post");
  assertEquals(thread.args[0], "777");
  assertStringIncludes(thread.args[1] as string, "User 9 (9)");
  assertStringIncludes(thread.args[1] as string, "title changed");
  assertStringIncludes(thread.args[1] as string, "issuecomment-55");
  assertEquals(t.of("renameThread")[0].args, ["777", "#12 [Bug]: Black window on Sway"]);
  assertStringIncludes(said(t.calls), "Updated the title, 1 field");
});

Deno.test("a moderator's edit of somebody else's report says so in the audit comment", async () => {
  const t = tracker();
  await openAndSubmit(
    t,
    { "cordial-title": "[Bug]: tidied", "what-happened": "The window opens and stays black." },
    { "cordial-labels": ["bug"] },
    MOD,
  );
  assertStringIncludes(t.of("comment")[0].args[1] as string, "(`mod`, as a moderator)");
  assertEquals(t.of("updateIssue").length, 1);
});

Deno.test("a stranger who forges the submit changes nothing", async () => {
  const t = tracker();
  // The dialog as the reporter was shown it, submitted by somebody who was not.
  const { response } = await handle(t.context, pressAs("cordial-edit-open:12"));
  const { after } = await handle(
    t.context,
    submission(modalOf(response), { "cordial-title": "[Bug]: vandalised" }, {}, asUser("666")),
  );
  await after!();
  assertEquals(t.of("comment").length, 0);
  assertEquals(t.of("updateIssue").length, 0);
  assertStringIncludes(said(t.calls), "Only the person who filed this report");
});

Deno.test("an edit from a dialog that has gone stale is refused and nothing is overwritten", async () => {
  const t = tracker();
  const { response } = await handle(t.context, pressAs("cordial-edit-open:12"));
  const modal = modalOf(response);
  // A maintainer retitles it while the dialog is open.
  t.issue.title = "[Bug]: a maintainer's better title";
  const { after } = await handle(
    t.context,
    submission(modal, { "cordial-title": "[Bug]: mine" }, { "cordial-labels": [] }),
  );
  await after!();
  assertEquals(t.of("updateIssue").length, 0);
  assertEquals(t.of("comment").length, 0);
  assertStringIncludes(said(t.calls), "changed after you opened the editor");
  assertStringIncludes(said(t.calls), "Nothing was saved");
});

Deno.test("if the audit comment cannot be written the edit does not happen", async () => {
  const t = tracker();
  (t.context.github as unknown as Json).comment = () =>
    Promise.reject(new Error("comment refused"));
  await openAndSubmit(t, { "cordial-title": "[Bug]: silent?" }, { "cordial-labels": [] });
  assertEquals(t.of("updateIssue").length, 0, "never silently overwrite");
  assertStringIncludes(said(t.calls), "nothing was changed");
  assertStringIncludes(said(t.calls), "comment refused");
});

Deno.test("if GitHub refuses the edit, the audit comment is amended to say it did not happen", async () => {
  const t = tracker();
  (t.context.github as unknown as Json).updateIssue = () => Promise.reject(new Error("422"));
  await openAndSubmit(t, { "cordial-title": "[Bug]: refused" }, { "cordial-labels": [] });
  const [amended] = t.of("editComment");
  assertEquals(amended.args[0], 55);
  assertStringIncludes(amended.args[1] as string, "This edit was not applied");
  assertEquals(t.of("post").length, 0, "nothing is announced in the thread");
  assertStringIncludes(said(t.calls), "nothing was changed");
});

Deno.test("saving without changing anything writes nothing at all", async () => {
  const t = tracker();
  await openAndSubmit(
    t,
    {
      "cordial-title": "[Bug]: The window opens and stays black.",
      "what-happened": "The window opens and stays black.",
      "what-expected": "The Roblox home screen.",
      "repro": "Launch it.",
    },
    { "cordial-labels": [] },
  );
  assertEquals(t.of("comment").length, 0);
  assertEquals(t.of("updateIssue").length, 0);
  assertStringIncludes(said(t.calls), "Nothing changed");
});

Deno.test("a required field cannot be emptied by an edit", async () => {
  const t = tracker();
  await openAndSubmit(t, { "cordial-title": "[Bug]: x", "repro": "   " }, { "cordial-labels": [] });
  assertEquals(t.of("updateIssue").length, 0);
  assertStringIncludes(said(t.calls), "How to reproduce");
  assertStringIncludes(said(t.calls), "required");
});

Deno.test("a field the submission leaves out is left alone, not emptied", async () => {
  const t = tracker();
  await openAndSubmit(t, { "cordial-title": "[Bug]: retitled" }, { "cordial-labels": [] });
  const body = (t.of("updateIssue")[0].args[1] as { body?: string }).body;
  assertEquals(body, undefined, "the body was not rewritten at all");
});

Deno.test("the reply offers the next part, and that dialog edits the later fields", async () => {
  const t = tracker();
  await openAndSubmit(t, { "cordial-title": "[Bug]: retitled" }, { "cordial-labels": [] });
  const reply = t.of("editOriginal").at(-1)!.args[1] as {
    components: { components: { custom_id: string }[] }[];
  };
  assertEquals(reply.components[0].components[0].custom_id, "cordial-edit-open:12:1");

  const modal = await openAndSubmit(
    t,
    { "engine-log": "Segfault in libroblox at frame 3." },
    undefined,
    asUser("9"),
    1,
  );
  assert(modal.components.length <= 5);
  assert(!inputs(modal).includes("cordial-title"), "later parts carry no title");
  const second = t.of("updateIssue").at(-1)!.args[1] as { body: string };
  assertStringIncludes(
    second.body,
    "### The engine's own log\n\nSegfault in libroblox at frame 3.",
  );
  assertStringIncludes(second.body, "Filed by **Someone** (`9`)");
  // One audit entry per part.
  assertEquals(t.of("comment").length, 2);
});

Deno.test("a reporter editing labels cannot add a triage label or strip the project's own", async () => {
  const t = tracker({ labelsOnIssue: ["bug", "confirmed", "area:input"] });
  await openAndSubmit(
    t,
    {},
    // area:input deselected, gpu:nvidia added, and a forged `wontfix` and `confirmed`.
    { "cordial-labels": ["gpu:nvidia", "wontfix", "priority: high"] },
  );
  const [update] = t.of("updateIssue");
  assertEquals((update.args[1] as { labels: string[] }).labels, ["bug", "confirmed", "gpu:nvidia"]);
  const audit = t.of("comment")[0].args[1] as string;
  assertStringIncludes(audit, "**Labels:** added `gpu:nvidia`; removed `area:input`");
});

Deno.test("a moderator editing labels may add any label, and it is logged the same way", async () => {
  const t = tracker({ labelsOnIssue: ["bug"] });
  await openAndSubmit(t, {}, { "cordial-labels": ["bug", "confirmed", "priority: high"] }, MOD);
  const [update] = t.of("updateIssue");
  assertEquals((update.args[1] as { labels: string[] }).labels, [
    "bug",
    "confirmed",
    "priority: high",
  ]);
  assertStringIncludes(
    t.of("comment")[0].args[1] as string,
    "added `confirmed`, `priority: high`",
  );
});

Deno.test("a label change when the list has gone is skipped, not guessed", async () => {
  const t = tracker();
  const { response } = await handle(t.context, pressAs("cordial-edit-open:12"));
  // The list vanishes between the dialog opening and being submitted.
  t.context.labels = { get: () => Promise.resolve(null) };
  const { after } = await handle(
    t.context,
    submission(modalOf(response), { "cordial-title": "[Bug]: kept" }, {
      "cordial-labels": ["gpu:nvidia"],
    }),
  );
  await after!();
  const [update] = t.of("updateIssue");
  assertEquals((update.args[1] as { labels?: string[] }).labels, undefined);
  assertStringIncludes(said(t.calls), "Labels were left as they were");
});

Deno.test("an issue whose body was reflowed on GitHub can still have its title and labels edited", async () => {
  const t = tracker();
  t.issue.body = t.issue.body.replace("\n\n### What you expected", "\n### What you expected");
  const { response } = await handle(t.context, pressAs("cordial-edit-open:12"));
  assertEquals(inputs(modalOf(response)), ["cordial-title", "cordial-labels"]);
  assertEquals(modalOf(response).title, "Edit #12", "one part only");
});

// ---- the picker is off unless asked for -----------------------------------------

Deno.test("with the picker off, every report dialog is exactly what it was before labels", async () => {
  for (const form of forms) {
    const t = tracker({ picker: false });
    let fetched = 0;
    t.context.labels = { get: () => (fetched++, Promise.resolve(repoLabels)) };
    const { response } = await handle(t.context, press(`cordial-issue-open:${form.slug}`, MOD));
    const modal = modalOf(response);
    // Same dialog as the plain generator: same id (no `:l`), same fields in
    // the same slots, and the optional fields that did not fit are the ones
    // that never did.
    assertEquals(modal, modalFor(form, "main") as Modal, form.slug);
    assertEquals(inputs(modal), form.placed.map((b) => b.id), form.slug);
    assertEquals(modal.custom_id, `cordial-issue:${form.slug}:main`);
    assertEquals(fetched, 0, "no label fetch is made for a dialog that has no picker");
  }
});

Deno.test("with the picker off, bug_report keeps its original five fields and four leftovers", async () => {
  const t = tracker({ picker: false });
  const { response } = await handle(t.context, press("cordial-issue-open:bug_report"));
  assertEquals(inputs(modalOf(response)), [
    "what-happened",
    "what-expected",
    "repro",
    "diagnostics",
    "engine-log",
  ]);
  const { after } = await handle(
    t.context,
    submission(
      { custom_id: "cordial-issue:bug_report:main" },
      { "what-happened": "x", "what-expected": "y", "repro": "z", "diagnostics": "d" },
    ),
  );
  await after!();
  const reply = t.of("editOriginal")[0].args[1] as {
    components: { components: { custom_id: string }[] }[];
  };
  assertEquals(reply.components[0].components[0].custom_id, "cordial-issue-extra:bug_report:12");
  assertEquals(t.of("createIssue")[0].args[2], ["bug"]);
});

Deno.test("picks submitted while the picker is off are ignored, not applied", async () => {
  // A dialog opened before the flag was turned off can still arrive.
  const t = tracker({ picker: false });
  const { after } = await handle(t.context, fileWithPicks(["gpu:nvidia"]));
  await after!();
  assertEquals(t.of("createIssue")[0].args[2], ["bug"]);
});

Deno.test("with the picker off, Edit changes title and text only", async () => {
  const t = tracker({ picker: false, labelsOnIssue: ["bug", "area:input"] });
  let fetched = 0;
  t.context.labels = { get: () => (fetched++, Promise.resolve(repoLabels)) };
  const { response } = await handle(t.context, pressAs("cordial-edit-open:12"));
  const modal = modalOf(response);
  assertEquals(inputs(modal), ["cordial-title", "what-happened", "what-expected", "repro"]);
  assert(!modal.custom_id.endsWith(":l"));
  assertEquals(fetched, 0);

  // A forged label selection on that dialog changes no labels.
  const { after } = await handle(
    t.context,
    submission(modal, { "cordial-title": "[Bug]: retitled" }, { "cordial-labels": ["gpu:nvidia"] }),
  );
  await after!();
  const [update] = t.of("updateIssue");
  assertEquals(update.args[1], { title: "[Bug]: retitled" });
});

Deno.test("an attachment-only message is carried to the issue as a link, with the expiry said", async () => {
  // The log archive is a .zip dropped into the thread. GitHub's API cannot
  // upload it to the issue, so the comment links the Discord copy.
  const { context, of } = fakes();
  (context.discord as unknown as { channelName: () => Promise<string> }).channelName = () =>
    Promise.resolve("#31 [Bug]: black window");

  const { after } = await handle(
    context,
    messageCommand({
      data: {
        name: "Add to the issue",
        type: 3,
        target_id: "m2",
        resolved: {
          messages: {
            m2: {
              content: "",
              author: { global_name: "Reporter" },
              attachments: [
                { filename: "cordial-logs.zip", url: "https://cdn.discordapp.com/attachments/1/2/cordial-logs.zip?ex=abc", size: 20480 },
                { filename: "evil.zip", url: "javascript:alert(1)" },
              ],
            },
          },
        },
      },
    }),
  );
  await after!();
  const [posted] = of("comment");
  const text = posted.args[1] as string;
  assertStringIncludes(text, "[cordial-logs.zip](https://cdn.discordapp.com/attachments/1/2/cordial-logs.zip?ex=abc) (20 KiB)");
  assertStringIncludes(text, "expire");
  assert(!text.includes("javascript:"), "only https links are carried");
});
