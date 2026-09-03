import { TEMPLATE_DIR } from "./repo.ts";
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@^1.0.8";
import { InteractionType, ResponseType } from "./discord.ts";
import { type Context, handle } from "./interactions.ts";
import { parseForm } from "./issue_forms.ts";
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
  ]);
});
