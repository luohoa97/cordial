import { assert, assertEquals, assertRejects } from "jsr:@std/assert@^1.0.8";
import { retryAfterMs, send } from "./resilient.ts";

const ok = () => new Response("fine", { status: 200 });
const status = (code: number, body = "", headers: HeadersInit = {}) =>
  new Response(body, { status: code, headers });

/** A `make` that returns the given responses in order, counting calls. */
function sequence(...responses: (() => Response | Promise<never>)[]) {
  let n = 0;
  return { make: () => Promise.resolve(responses[n++]()), calls: () => n };
}

Deno.test("a 429 is waited out and retried, whatever the call does", async () => {
  // The key asymmetry: a 429 means the request was *refused*, so repeating it
  // cannot duplicate anything -- it is retried even for a non-idempotent call.
  const s = sequence(() =>
    status(429, JSON.stringify({ retry_after: 0.01 }), {
      "content-type": "application/json",
    }), ok);
  const response = await send(s.make, "post", { idempotent: false });
  assertEquals(response.status, 200);
  assertEquals(s.calls(), 2);
});

Deno.test("the wait comes from what they told us, in either dialect", async () => {
  assertEquals(
    await retryAfterMs(status(429, JSON.stringify({ retry_after: 2.5 }), {
      "content-type": "application/json",
    })),
    2500,
    "Discord puts seconds in the body",
  );
  assertEquals(
    await retryAfterMs(status(429, "", { "retry-after": "3" })),
    3000,
    "GitHub uses the standard header",
  );
  assertEquals(await retryAfterMs(status(500, "nope")), null);
});

Deno.test("a 5xx is retried only where a repeat is safe", async () => {
  const safe = sequence(() => status(503), ok);
  assertEquals((await send(safe.make, "read", { idempotent: true })).status, 200);
  assertEquals(safe.calls(), 2);

  // **This is the rule that stops one report becoming two issues.** A 5xx may
  // have committed the write; repeating it blindly would file twice.
  const unsafe = sequence(() => status(503), ok);
  assertEquals((await send(unsafe.make, "create", { idempotent: false })).status, 503);
  assertEquals(unsafe.calls(), 1, "a create must not be repeated on an unknown outcome");
});

Deno.test("a 4xx is returned at once, because it will fail again", async () => {
  const s = sequence(() => status(403, "forbidden"), ok);
  assertEquals((await send(s.make, "x", { idempotent: true })).status, 403);
  assertEquals(s.calls(), 1);
});

Deno.test("a dropped connection is retried only where a repeat is safe", async () => {
  const boom = () => Promise.reject(new TypeError("network"));

  const safe = sequence(boom, ok);
  assertEquals((await send(safe.make, "read", { idempotent: true })).status, 200);
  assertEquals(safe.calls(), 2);

  const unsafe = sequence(boom, ok);
  await assertRejects(() => send(unsafe.make, "create", { idempotent: false }));
  assertEquals(unsafe.calls(), 1);
});

Deno.test("retrying gives up rather than looping", async () => {
  let calls = 0;
  const make = () => {
    calls++;
    return Promise.resolve(status(429, JSON.stringify({ retry_after: 0.001 }), {
      "content-type": "application/json",
    }));
  };
  const response = await send(make, "x", { idempotent: true, attempts: 3 });
  assertEquals(response.status, 429, "the last response is returned, not thrown");
  assertEquals(calls, 3);
});

Deno.test("a first-time success costs exactly one call", async () => {
  // The control for all of the above: nothing is retried that did not fail.
  const s = sequence(ok);
  assert((await send(s.make, "x", { idempotent: true })).ok);
  assertEquals(s.calls(), 1);
});
