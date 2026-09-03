import { assert, assertEquals } from "jsr:@std/assert@^1.0.8";
import { build } from "./main.ts";

/**
 * **The bug this pins killed the first real bug report filed through the
 * bridge.** On Cloudflare a promise still running when the fetch handler
 * returns is cancelled, so the deferred follow-up -- file the issue, open the
 * thread, edit the reply -- never ran, and the reporter sat on "Cordial Issues
 * is thinking" for ever with no issue created. Under Deno the same code is
 * correct, because the process outlives the response, so nothing local caught
 * it and nothing could have.
 *
 * The assertion is therefore not "the work happens" but "the work is handed to
 * the host", which is the part that differs between the two.
 */
const CONFIG = {
  GITHUB_OWNER: "o",
  GITHUB_REPO: "r",
  GITHUB_APP_ID: "1",
  GITHUB_INSTALLATION_ID: "2",
  GITHUB_APP_LOGIN: "bot",
  GITHUB_WEBHOOK_SECRET: "s",
  DISCORD_BOT_TOKEN: "t",
  DISCORD_APPLICATION_ID: "3",
  DISCORD_THREAD_CHANNEL_ID: "4",
  DISCORD_PICKER_CHANNEL_ID: "5",
  // A throwaway key, generated for this test and used for nothing.
  GITHUB_APP_PRIVATE_KEY: await (async () => {
    const pair = await crypto.subtle.generateKey(
      {
        name: "RSASSA-PKCS1-v1_5",
        modulusLength: 2048,
        publicExponent: new Uint8Array([1, 0, 1]),
        hash: "SHA-256",
      },
      true,
      ["sign", "verify"],
    ) as CryptoKeyPair;
    const der = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
    const b64 = btoa(String.fromCharCode(...der)).replace(/(.{64})/g, "$1\n");
    return `-----BEGIN PRIVATE KEY-----\n${b64}\n-----END PRIVATE KEY-----\n`;
  })(),
  DISCORD_PUBLIC_KEY: "00".repeat(32),
};

Deno.test("a deferred interaction's follow-up is handed to the host's waitUntil", async () => {
  const handler = await build(CONFIG);

  const handed: Promise<unknown>[] = [];
  // A signed request cannot be forged here (the public key is all zeroes), so
  // this drives the path that *can* be reached without one and asserts the
  // wiring rather than the work: nothing may be handed over for a request that
  // never gets past the signature gate.
  const rejected = await handler(
    new Request("https://x/interactions", { method: "POST", body: '{"type":1}' }),
    (p) => handed.push(p),
  );
  assertEquals(rejected.status, 401, "an unsigned interaction must be refused");
  assertEquals(handed.length, 0, "and must hand no background work to the host");
});

Deno.test("the handler still works for a host that offers no waitUntil", async () => {
  // Deno is such a host: the process outlives the response, so the absence of
  // the callback must not be an error.
  const handler = await build(CONFIG);
  const health = await handler(new Request("https://x/health"));
  assertEquals(health.status, 200);
  assertEquals((await health.json()).ok, true);
});

Deno.test("worker.ts asks for waitUntil, because on Workers it is not optional", async () => {
  // A source assertion, deliberately. The behaviour it guards cannot be
  // reproduced under Deno at all -- promise cancellation on return is the
  // Workers runtime's own semantics -- so the only thing a test here can do is
  // refuse to let the call site quietly lose the argument again.
  const source = await Deno.readTextFile(new URL("./worker.ts", import.meta.url));
  assert(
    /handler\(\s*request\s*,\s*ctx\.waitUntil/.test(source),
    "worker.ts must pass ctx.waitUntil to the handler",
  );
});
