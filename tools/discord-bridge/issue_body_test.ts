import { TEMPLATE_DIR } from "./repo.ts";
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@^1.0.8";
import { parseForm } from "./issue_forms.ts";
import {
  assembleBody,
  changedFields,
  fieldSection,
  formFromBody,
  isEditable,
  parseBody,
  renderIssueBody,
  renderIssueTitle,
  reporterFromBody,
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

// ---- editing ---------------------------------------------------------------

const filed = renderIssueBody(form, submission, "999");

Deno.test("the marker records the form, the reporter and the thread, and old markers still read", () => {
  assertEquals(formFromBody(filed), "bug_report");
  assertEquals(reporterFromBody(filed), "42");
  assertEquals(threadFromBody(filed), "999");
  // Issues filed before the form was recorded have neither key and must not
  // stop being pairable or closable.
  const old = threadMarker("77", "5");
  assertEquals(formFromBody(old), null);
  assertEquals(reporterFromBody(old), "5");
  assertEquals(threadFromBody(old), "77");
});

Deno.test("a body the bridge wrote parses back and re-assembles to itself", () => {
  const parsed = parseBody(form, filed);
  assert(parsed, "a freshly filed body must be editable");
  assertEquals(assembleBody(form, parsed, {}), filed);
  assertEquals(parsed.sections.get("what-happened")?.value, submission.values["what-happened"]);
  assertStringIncludes(parsed.tail, "### Reported from Discord");
  assertStringIncludes(parsed.tail, "reporter=42");
});

Deno.test("editing one field changes that section and nothing else", () => {
  const parsed = parseBody(form, filed)!;
  const next = assembleBody(form, parsed, { "what-happened": "It crashes on launch." });
  assertStringIncludes(next, "### What happened\n\nIt crashes on launch.");
  assert(!next.includes("stays black"));
  // Everything else is byte for byte what it was.
  assertStringIncludes(next, "### Diagnostics\n\nCordial 0.13.2 (91f8ee9)\nInstall rpm");
  assertStringIncludes(next, "### What you expected");
  assertEquals(
    changedFields(parsed, form, { "what-happened": "It crashes on launch." }).map((c) =>
      c.block.id
    ),
    ["what-happened"],
  );
});

Deno.test("the diagnostics block, the credit line and the marker survive any edit", () => {
  const parsed = parseBody(form, filed)!;
  assert(!isEditable(form.fields.find((b) => b.id === "diagnostics")!));
  // An attempt to edit diagnostics through the same path is ignored, because
  // the field is not editable.
  const next = assembleBody(form, parsed, {
    "diagnostics": "Cordial 9.9.9 (forged)",
    "repro": "Different steps.",
  });
  assert(!next.includes("forged"));
  assertStringIncludes(next, "Cordial 0.13.2 (91f8ee9)");
  assertStringIncludes(next, "Filed by **someone** (`42`)");
  assertEquals(reporterFromBody(next), "42");
  assertEquals(threadFromBody(next), "999");
  assertEquals(formFromBody(next), "bug_report");
  assert(next.endsWith("-->"), "the marker is still last");
});

Deno.test("saving a box unchanged leaves its section exactly as it was", () => {
  // Including a truncation note, which an edit that rebuilt the section would
  // lose -- and with it the warning that the end of a log is missing.
  const withLog = renderIssueBody(form, {
    ...submission,
    values: { ...submission.values, "engine-log": "x".repeat(4000) },
  }, "999");
  assertStringIncludes(withLog, "4000-character limit");
  const parsed = parseBody(form, withLog)!;
  assertEquals(parsed.sections.get("engine-log")?.value, "x".repeat(4000));
  const next = assembleBody(form, parsed, { "engine-log": "x".repeat(4000) });
  assertEquals(next, withLog);
  assertEquals(changedFields(parsed, form, { "engine-log": "x".repeat(4000) }), []);
});

Deno.test("a blank answer removes the section, and a new one lands in template order", () => {
  const parsed = parseBody(form, filed)!;
  const removed = assembleBody(form, parsed, { "repro": "" });
  assert(!removed.includes("### How to reproduce"), removed);

  // `runs-attempted` comes after diagnostics in the template, and was empty
  // when this was filed.
  const added = assembleBody(form, parsed, { "runs-attempted": "Three times." });
  const heads = [...added.matchAll(/^### .*/gm)].map((m) => m[0]);
  assertEquals(heads, [
    "### What happened",
    "### What you expected",
    "### How to reproduce",
    "### Diagnostics",
    "### Runs attempted",
    "### Reported from Discord",
  ]);
  // And what was filed is still re-parseable after the edit.
  assert(parseBody(form, added), "an edited body must stay editable");
});

Deno.test("GitHub's own `_No response_` counts as empty and is kept if left alone", () => {
  const web = "### What happened\n\nIt broke.\n\n### Repro\n\n_No response_";
  const tiny = parseForm(
    "t",
    "name: t\nbody:\n  - type: textarea\n    id: what\n    attributes:\n      label: What happened\n" +
      "  - type: textarea\n    id: repro\n    attributes:\n      label: Repro\n",
  );
  const parsed = parseBody(tiny, web)!;
  assert(parsed);
  assertEquals(changedFields(parsed, tiny, { repro: "" }), [], "empty to empty is no change");
  assertEquals(assembleBody(tiny, parsed, { repro: "" }), web);
});

Deno.test("a body somebody reflowed on GitHub is refused rather than guessed at", () => {
  assertEquals(
    parseBody(form, filed.replace("\n\n### What you expected", "\n### What you expected")),
    null,
  );
  assertEquals(parseBody(form, "Intro a maintainer added.\n\n" + filed), null);
});

Deno.test("a note a maintainer appended below the marker rides through an edit", () => {
  // The tail is carried verbatim, so what is added after it is not lost.
  const noted = filed + "\n\nMaintainer: seen on two machines.";
  const parsed = parseBody(form, noted)!;
  assert(parsed);
  const next = assembleBody(form, parsed, { "repro": "New steps." });
  assertStringIncludes(next, "Maintainer: seen on two machines.");
  assertEquals(reporterFromBody(next), "42");
});

Deno.test("a marker inside the editable part is never offered for editing", () => {
  const poisoned = filed.replace(
    "The window opens and stays black.",
    `The window opens.\n${threadMarker("1", "7")}`,
  );
  assertEquals(parseBody(form, poisoned), null);
});

Deno.test("a pasted heading inside an answer is content, not a boundary", () => {
  const sneaky = renderIssueBody(form, {
    ...submission,
    values: { ...submission.values, "what-happened": "Look:\n### Diagnostics\nnot the real one" },
  }, "999");
  const parsed = parseBody(form, sneaky);
  // Either it parses with the pasted line kept inside the answer, or it is
  // refused; what it must not do is treat it as the Diagnostics section.
  if (parsed) {
    assertStringIncludes(parsed.sections.get("what-happened")!.value, "not the real one");
    assertStringIncludes(parsed.sections.get("diagnostics")!.value, "Cordial 0.13.2");
  }
});

Deno.test("a Discord report says no log archive is attached, and how one gets added", () => {
  const body = renderIssueBody(form, submission, "999");
  assertStringIncludes(body, "No log archive is attached");
  assertStringIncludes(body, "Save logs");
  // The note sits inside the section the editor carries through untouched.
  assert(body.indexOf("No log archive") > body.indexOf("### Reported from Discord"));
  // And a body round-trips as before.
  assert(parseBody(form, body) !== null);
});
