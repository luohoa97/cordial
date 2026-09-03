/**
 * The signature check is the bridge's whole authentication, so it is tested
 * the way a lock is tested: not "does the right key open it" alone, but "does
 * anything else". Every case below that should fail has a matching case that
 * should pass, because a verifier that rejects everything passes a
 * negative-only suite.
 */
import { assert, assertEquals, assertFalse } from "jsr:@std/assert@^1.0.8";
import { importPublicKey, verifyRequest } from "./verify.ts";

function hex(bytes: Uint8Array): string {
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

async function signed(body: string, timestamp: string) {
  const pair = await crypto.subtle.generateKey({ name: "Ed25519" }, true, [
    "sign",
    "verify",
  ]) as CryptoKeyPair;
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  const signature = new Uint8Array(
    await crypto.subtle.sign(
      { name: "Ed25519" },
      pair.privateKey,
      new TextEncoder().encode(timestamp + body),
    ),
  );
  return { key: await importPublicKey(hex(raw)), signature: hex(signature) };
}

Deno.test("a request Discord really signed verifies", async () => {
  const body = '{"type":1}';
  const { key, signature } = await signed(body, "1700000000");
  assert(await verifyRequest(key, signature, "1700000000", body));
});

Deno.test("a changed body does not verify", async () => {
  const { key, signature } = await signed('{"type":1}', "1700000000");
  assertFalse(await verifyRequest(key, signature, "1700000000", '{"type":2}'));
});

Deno.test("a replayed signature under a different timestamp does not verify", async () => {
  const body = '{"type":1}';
  const { key, signature } = await signed(body, "1700000000");
  assertFalse(await verifyRequest(key, signature, "1700000001", body));
});

Deno.test("another key's signature does not verify", async () => {
  const body = '{"type":1}';
  const mine = await signed(body, "1700000000");
  const theirs = await signed(body, "1700000000");
  assertFalse(await verifyRequest(mine.key, theirs.signature, "1700000000", body));
});

Deno.test("a flipped bit in the signature does not verify", async () => {
  const body = '{"type":1}';
  const { key, signature } = await signed(body, "1700000000");
  const flipped = signature.slice(0, -1) +
    (signature.at(-1) === "0" ? "1" : "0");
  assertFalse(await verifyRequest(key, flipped, "1700000000", body));
});

Deno.test("missing, malformed and wrong-length signatures are refused, not thrown", async () => {
  const { key } = await signed("{}", "1");
  for (const bad of [null, "", "zz", "abc", "ab".repeat(63), "ab".repeat(65)]) {
    assertEquals(
      await verifyRequest(key, bad, "1", "{}"),
      false,
      `signature ${JSON.stringify(bad)} must be refused`,
    );
  }
  assertFalse(await verifyRequest(key, "ab".repeat(64), null, "{}"));
});

Deno.test("the raw body is what is signed, not a re-serialised copy", async () => {
  // Two JSON texts that parse to the same object. A verifier that re-encoded
  // before checking would accept both; Discord signs bytes, so only one is
  // genuine and the other must fail.
  const genuine = '{"a":1,"b":2}';
  const reordered = '{"b":2,"a":1}';
  const { key, signature } = await signed(genuine, "1700000000");
  assert(await verifyRequest(key, signature, "1700000000", genuine));
  assertFalse(await verifyRequest(key, signature, "1700000000", reordered));
});
