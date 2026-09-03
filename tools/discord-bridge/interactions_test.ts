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
