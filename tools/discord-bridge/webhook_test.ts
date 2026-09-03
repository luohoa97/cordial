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
