import { assert, assertEquals, assertFalse, assertStringIncludes } from "jsr:@std/assert@^1.0.8";
import { relayFor, verifyGitHubSignature } from "./webhook.ts";

const SECRET = "not-the-real-one";

async function sign(body: string): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(SECRET),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const mac = new Uint8Array(await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(body)));
  return "sha256=" + [...mac].map((b) => b.toString(16).padStart(2, "0")).join("");
}

Deno.test("a webhook GitHub signed verifies, and nothing else does", async () => {
  const body = '{"action":"created"}';
  assert(await verifyGitHubSignature(SECRET, await sign(body), body));
  assertFalse(await verifyGitHubSignature(SECRET, await sign(body), body + " "));
  assertFalse(await verifyGitHubSignature("another secret", await sign(body), body));
  for (const bad of [null, "", "sha1=abc", "sha256=zz", "sha256=" + "a".repeat(63)]) {
    assertFalse(await verifyGitHubSignature(SECRET, bad, body), `${bad} must be refused`);
  }
});

const paired = {
  action: "created",
  issue: { number: 7, body: "text\n<!-- cordial-bridge thread=555 -->" },
  comment: { body: "Fixed in 0.13.2.", user: { login: "maintainer" }, html_url: "https://x/1" },
};

Deno.test("a real comment on a paired issue relays to its thread", () => {
  const relay = relayFor(paired, "cordial-bridge");
  assert(relay);
  assertEquals(relay.threadId, "555");
  assertStringIncludes(relay.content, "maintainer");
  assertStringIncludes(relay.content, "Fixed in 0.13.2.");
});

Deno.test("the bridge's own comment does not come back round", () => {
  // The loop this cuts is real: a Discord user's comment is posted by the App,
  // arrives here as an event, and would be echoed into the thread it came from.
  for (const login of ["cordial-bridge", "cordial-bridge[bot]"]) {
    assertEquals(
      relayFor({ ...paired, comment: { ...paired.comment, user: { login } } }, "cordial-bridge"),
      null,
      `${login} must not be relayed`,
    );
  }
});

Deno.test("events that are not a new comment on a paired issue are ignored", () => {
  assertEquals(relayFor({ ...paired, action: "edited" }, "bridge"), null);
  assertEquals(relayFor({ ...paired, issue: { number: 7, body: "no marker" } }, "bridge"), null);
  assertEquals(
    relayFor({ ...paired, comment: { ...paired.comment, body: "   " } }, "bridge"),
    null,
  );
  assertEquals(relayFor({}, "bridge"), null);
});

Deno.test("a very long comment is trimmed, because the issue is where it lives", () => {
  const relay = relayFor(
    { ...paired, comment: { ...paired.comment, body: "y".repeat(5000) } },
    "bridge",
  )!;
  assert(relay.content.length < 2000, `${relay.content.length} characters`);
  assertStringIncludes(relay.content, "…");
});

const closedEvent = {
  action: "closed",
  issue: {
    number: 7,
    body: "text\n<!-- cordial-bridge thread=555 reporter=42 -->",
    html_url: "https://github.com/o/r/issues/7",
    state_reason: "completed",
  },
  sender: { login: "maintainer" },
};

Deno.test("a close on GitHub reaches the thread and archives it", () => {
  const relay = relayFor(closedEvent, "cordial-bridge", "issues");
  assert(relay);
  assertEquals(relay.threadId, "555");
  assertEquals(relay.archive, true, "the thread follows the issue");
  assertStringIncludes(relay.content, "maintainer");
  assertStringIncludes(relay.content, "closed as completed");
});

Deno.test("closing as not planned reads differently from fixing it", () => {
  // The two are different facts and the thread should not blur them: one
  // invites "actually it is still happening", the other invites "that is
  // wrong".
  const relay = relayFor(
    { ...closedEvent, issue: { ...closedEvent.issue, state_reason: "not_planned" } },
    "cordial-bridge",
    "issues",
  )!;
  assertStringIncludes(relay.content, "closed #7");
  assert(!relay.content.includes("completed"), relay.content);
});

Deno.test("a reopen brings the thread back rather than archiving it", () => {
  const relay = relayFor(
    { ...closedEvent, action: "reopened" },
    "cordial-bridge",
    "issues",
  )!;
  assertEquals(relay.archive, false);
  assertStringIncludes(relay.content, "reopened #7");
});

Deno.test("the bridge's own close does not come back round", () => {
  // Closing from the Discord button already posts in the thread; relaying the
  // webhook it causes would say it twice.
  for (const login of ["cordial-bridge", "cordial-bridge[bot]"]) {
    assertEquals(
      relayFor({ ...closedEvent, sender: { login } }, "cordial-bridge", "issues"),
      null,
      `${login} must not be relayed`,
    );
  }
});

Deno.test("state changes on an unpaired issue, and other actions, are ignored", () => {
  assertEquals(
    relayFor({ ...closedEvent, issue: { number: 7, body: "no marker" } }, "b", "issues"),
    null,
  );
  assertEquals(relayFor({ ...closedEvent, action: "labeled" }, "b", "issues"), null);
  // And an event the bridge is not subscribed to must not be guessed at.
  assertEquals(relayFor(closedEvent, "b", "pull_request"), null);
});

Deno.test("a comment event still routes as a comment, not a state change", () => {
  const relay = relayFor(paired, "cordial-bridge", "issue_comment")!;
  assertEquals(relay.archive, undefined, "a comment must not touch the thread's state");
  assertStringIncludes(relay.content, "commented");
});
