import { TEMPLATE_DIR } from "./repo.ts";
import { assert, assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@^1.0.8";
import { Templates } from "./templates.ts";

const GOOD = Deno.readTextFileSync(`${TEMPLATE_DIR}/bug_report.yml`);
const BROKEN = `
name: Broken by an edit
body:
${
  ["a", "b", "c", "d", "e", "f"].map((id) =>
    `  - type: textarea
    id: ${id}
    attributes: {label: Field ${id}}
    validations: {required: true}`
  ).join("\n")
}
`;

function harness(sequence: string[]) {
  let call = 0;
  let clock = 0;
  const templates = new Templates(
    { owner: "o", repo: "r", ref: "main" },
    {
      now: () => clock,
      read: () => Promise.resolve(sequence[Math.min(call++, sequence.length - 1)]),
    },
  );
  return { templates, tick: (ms: number) => clock += ms, calls: () => call };
}

Deno.test("the forms are cached, so a busy channel costs one fetch", async () => {
  const h = harness([GOOD]);
  await h.templates.forms(["bug_report.yml"]);
  await h.templates.forms(["bug_report.yml"]);
  await h.templates.forms(["bug_report.yml"]);
  assertEquals(h.calls(), 1);
});

Deno.test("the cache expires, so an edit reaches Discord without a redeploy", async () => {
  const h = harness([GOOD]);
  await h.templates.forms(["bug_report.yml"]);
  h.tick(6 * 60 * 1000);
  await h.templates.forms(["bug_report.yml"]);
  assertEquals(h.calls(), 2);
});

Deno.test("a template edited into a shape the modal cannot hold keeps the last good set", async () => {
  // This is the whole reason fetching at runtime is safe. Without it, a bad
  // edit takes the buttons down in front of whoever presses one next.
  const h = harness([GOOD, BROKEN]);
  const before = await h.templates.forms(["bug_report.yml"]);
  assertEquals(h.templates.stale, undefined);

  h.tick(6 * 60 * 1000);
  const after = await h.templates.forms(["bug_report.yml"]);

  assertEquals(after, before, "the good forms must still be served");
  assert(h.templates.stale, "and the bridge must say it is behind");
  assertStringIncludes(h.templates.stale!, "do not fit");
});

Deno.test("a failure before any good set has been seen is not swallowed", async () => {
  // Nothing to fall back to means there is nothing to serve, and pretending
  // otherwise would hand out an empty picker with no explanation.
  const h = harness([BROKEN]);
  await assertRejects(() => h.templates.forms(["bug_report.yml"]));
});

Deno.test("a transient failure is retried on the next interaction, not pinned for the TTL", async () => {
  const h = harness([GOOD, BROKEN, GOOD]);
  await h.templates.forms(["bug_report.yml"]);
  h.tick(6 * 60 * 1000);
  await h.templates.forms(["bug_report.yml"]); // rejected, serves stale
  assert(h.templates.stale);
  await h.templates.forms(["bug_report.yml"]); // immediately tries again
  assertEquals(h.calls(), 3, "the bad fetch must not have refreshed the clock");
});
