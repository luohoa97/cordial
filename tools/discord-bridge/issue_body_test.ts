import { TEMPLATE_DIR } from "./repo.ts";
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@^1.0.8";
import { parseForm } from "./issue_forms.ts";
import {
  fieldSection,
  renderIssueBody,
  renderIssueTitle,
  threadFromBody,
  threadMarker,
} from "./issue_body.ts";

const form = parseForm(
  "bug_report",
  Deno.readTextFileSync(`${TEMPLATE_DIR}/bug_report.yml`),
);

const submission = {
  values: {
    "what-happened": "The window opens and stays black.\nEvery time.",
    "what-expected": "The Roblox home screen.",
    "repro": "Launch it.",
    "diagnostics": "Cordial 0.13.2 (91f8ee9)\nInstall rpm",
  },
  reporter: { id: "42", tag: "someone" },
};

Deno.test("the body reads like one GitHub's own form renderer produced", () => {
  const body = renderIssueBody(form, submission, "999");
  // Same `### Label` shape, so a maintainer cannot tell which route a report
  // took without looking for the note that says so.
  assertStringIncludes(body, "### What happened");
  assertStringIncludes(body, "The window opens and stays black.");
  assertStringIncludes(body, "### Diagnostics");
  assertStringIncludes(body, "### Reported from Discord");
  assertStringIncludes(body, "someone");
});

Deno.test("a field left blank leaves no empty heading behind", () => {
  const body = renderIssueBody(form, {
    ...submission,
    values: { ...submission.values, repro: "  " },
  }, null);
  assert(!body.includes("### How to reproduce"), body);
});

Deno.test("the thread marker survives a round trip and is invisible in Markdown", () => {
  const body = renderIssueBody(form, submission, "123456789012345678");
  assertEquals(threadFromBody(body), "123456789012345678");
  // An HTML comment, so GitHub renders nothing for it.
  assertStringIncludes(body, "<!--");
  assertEquals(threadFromBody("no marker here"), null);
  assertEquals(threadFromBody(null), null);
});

Deno.test("a user editing around the marker does not break the pairing", () => {
  // The pairing lives in the artefact, so the artefact gets edited. This is
  // the case that would otherwise silently orphan a thread.
  const edited = `Some text a maintainer added.\n\n${threadMarker("77")}\n\nAnd more after it.`;
  assertEquals(threadFromBody(edited), "77");
});

Deno.test("the title keeps the template's prefix and stays one line", () => {
  const title = renderIssueTitle(form, submission);
  assert(title.startsWith("[Bug]: "), title);
  assertEquals(title, "[Bug]: The window opens and stays black.");
  assert(!title.includes("\n"));
});

Deno.test("a very long first answer is trimmed rather than sent whole", () => {
  const long = {
    ...submission,
    values: { ...submission.values, "what-happened": "x".repeat(500) },
  };
  const title = renderIssueTitle(form, long);
  assert(title.length <= 120, `title is ${title.length} characters`);
  assert(title.endsWith("…"), title.slice(-10));
});

Deno.test("an answer that filled Discord's field says so, and a shorter one does not", () => {
  // Issue #28 arrived with the crash log's *beginning*: the startup banner
  // intact, the exit status and last frames gone, and nothing anywhere saying
  // it was cut. It read as a client that stopped for no reason.
  const log = form.fields.find((b) => b.id === "engine-log");
  assert(log, "bug_report should still have an engine-log field");

  // Exactly full, which is the only state that proves truncation.
  const full = renderIssueBody(form, {
    ...submission,
    values: { ...submission.values, "engine-log": "x".repeat(4000) },
  }, null);
  assertStringIncludes(full, "4000-character limit");
  assertStringIncludes(full, "**beginning**");

  // #28's own shape: 4000 in the box, 3996 after trailing whitespace comes
  // off. Testing the trimmed length would miss this, which is why the check
  // is on the raw value.
  const trailing = renderIssueBody(form, {
    ...submission,
    values: { ...submission.values, "engine-log": "x".repeat(3996) + "\n\n\n\n" },
  }, null);
  assertStringIncludes(trailing, "4000-character limit");

  // One short of full is not full. A note on an answer that merely came close
  // is noise, and noise stops being read.
  const roomy = renderIssueBody(form, {
    ...submission,
    values: { ...submission.values, "engine-log": "y".repeat(3999) },
  }, null);
  assert(!roomy.includes("character limit"), roomy);

  // Per field, not per body, and short fields carry their own lower cap.
  assertEquals(full.match(/character limit/g)?.length, 1);
  const short = form.fields.find((b) => b.type === "input" && b.id);
  assert(short);
  assertStringIncludes(fieldSection(short, "z".repeat(1000)), "1000-character limit");

  // Ordinary answers collect nothing.
  assert(!renderIssueBody(form, submission, null).includes("character limit"));
});
