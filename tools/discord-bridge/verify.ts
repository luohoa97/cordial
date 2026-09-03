/**
 * Discord's request signature, which is the whole of the bridge's authentication.
 *
 * An HTTP interactions endpoint is a public URL that creates GitHub issues.
 * The **only** thing standing between it and anybody who finds the URL is this
 * check: Discord signs every request with Ed25519 over `timestamp + body` and
 * the application's public key verifies it. Get it wrong in the permissive
 * direction and the tracker is an open write endpoint.
 *
 * So the rules here are deliberately blunt:
 *
 *   - **verify over the raw body bytes**, never a re-serialised object. JSON
 *     round-tripping reorders keys and rewrites numbers, and the signature is
 *     over what was sent;
 *   - **any malformed input is a failure, not an exception to handle**. A bad
 *     hex string, a missing header and a wrong key all return false;
 *   - Discord requires a `401` for a bad signature specifically, and will
 *     refuse to register an endpoint that does not do it.
 */

function fromHex(hex: string): Uint8Array<ArrayBuffer> | null {
  if (hex.length === 0 || hex.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(hex)) {
    return null;
  }
  // Backed by a plain ArrayBuffer explicitly: Web Crypto's `BufferSource` will
  // not take the `ArrayBufferLike` a bare `new Uint8Array(n)` is typed as.
  const out = new Uint8Array(new ArrayBuffer(hex.length / 2));
  for (let i = 0; i < out.length; i++) {
    out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

export async function importPublicKey(publicKeyHex: string): Promise<CryptoKey> {
  const raw = fromHex(publicKeyHex);
  if (!raw) throw new Error("the application public key is not hex");
  return await crypto.subtle.importKey("raw", raw, { name: "Ed25519" }, false, [
    "verify",
  ]);
}

/**
 * True only for a request Discord really signed.
 *
 * `body` is the raw request text, exactly as received.
 */
export async function verifyRequest(
  key: CryptoKey,
  signatureHex: string | null,
  timestamp: string | null,
  body: string,
): Promise<boolean> {
  if (!signatureHex || !timestamp) return false;
  const signature = fromHex(signatureHex);
  if (!signature || signature.length !== 64) return false;
  try {
    return await crypto.subtle.verify(
      { name: "Ed25519" },
      key,
      signature,
      new TextEncoder().encode(timestamp + body),
    );
  } catch {
    return false;
  }
}
