import { assert, assertEquals } from "jsr:@std/assert@^1.0.8";
import { container, IS_COMPONENTS_V2, separator, text, v2 } from "./components.ts";

/**
 * **The invariant that broke every thread for a release.** Discord refuses
 * `content` alongside `IS_COMPONENTS_V2` outright, and the failure surfaced as
 * "the thread could not be opened" long after the issue had been filed. A
 * message is one shape or the other, never half of each.
 */
Deno.test("a V2 body carries no legacy fields", () => {
  const body = v2([container(0x1, [text("hi")])]) as Record<string, unknown>;
  assertEquals(body.flags, IS_COMPONENTS_V2);
  assert(!("content" in body), "content is refused alongside the V2 flag");
  assert(!("embeds" in body), "and so are embeds");
});

Deno.test("a separator can be a gap as well as a line", () => {
  assertEquals(separator(false, 2), { type: 14, divider: false, spacing: 2 });
});
