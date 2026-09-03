/**
 * The App key and the JWT it signs.
 *
 * The PKCS#1 wrapping is the part worth testing and the easy part to fake: a
 * test that derived the PKCS#1 form by inverting `pkcs1ToPkcs8` would be
 * checking the code against itself. So `openssl` is the oracle -- it emits
 * both encodings of one key -- and the assertion is that both import and that
 * what they sign verifies against the *same* public key.
 */
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@^1.0.8";
import { appJwt, importAppKey } from "./github.ts";

async function run(args: string[], stdin?: string): Promise<string> {
  const command = new Deno.Command("openssl", {
    args,
    stdin: stdin === undefined ? "null" : "piped",
    stdout: "piped",
    stderr: "piped",
  });
  const child = command.spawn();
  if (stdin !== undefined) {
    const writer = child.stdin.getWriter();
    await writer.write(new TextEncoder().encode(stdin));
    await writer.close();
  }
  const { code, stdout, stderr } = await child.output();
  if (code !== 0) throw new Error(`openssl ${args.join(" ")}: ${new TextDecoder().decode(stderr)}`);
  return new TextDecoder().decode(stdout);
}

async function keyPair() {
  const pkcs8 = await run(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048"]);
  const pkcs1 = await run(["rsa", "-traditional"], pkcs8);
  const publicPem = await run(["rsa", "-pubout"], pkcs8);
  return { pkcs8, pkcs1, publicPem };
}

function derFromPem(pem: string): Uint8Array<ArrayBuffer> {
  const binary = atob(pem.replace(/-----[^-]+-----/g, "").replace(/\s+/g, ""));
  const out = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

function unb64url(text: string): Uint8Array<ArrayBuffer> {
  const padded = text.replace(/-/g, "+").replace(/_/g, "/") +
    "=".repeat((4 - (text.length % 4)) % 4);
  const binary = atob(padded);
  const out = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

Deno.test("both PEM encodings GitHub and openssl produce are accepted", async () => {
  const { pkcs8, pkcs1, publicPem } = await keyPair();
  assertStringIncludes(pkcs1, "BEGIN RSA PRIVATE KEY", "the oracle must emit PKCS#1");
  assertStringIncludes(pkcs8, "BEGIN PRIVATE KEY", "the oracle must emit PKCS#8");

  const verifier = await crypto.subtle.importKey(
    "spki",
    derFromPem(publicPem),
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["verify"],
  );

  // The real assertion: a JWT signed through *either* import verifies against
  // the one public key. A wrapping that produced a different key would import
  // cleanly and fail here, which is the failure mode worth catching.
  for (const [shape, pem] of [["PKCS#8", pkcs8], ["PKCS#1", pkcs1]] as const) {
    const jwt = await appJwt("123456", await importAppKey(pem), 1_700_000_000_000);
    const [head, body, signature] = jwt.split(".");
    assert(
      await crypto.subtle.verify(
        "RSASSA-PKCS1-v1_5",
        verifier,
        unb64url(signature),
        new TextEncoder().encode(`${head}.${body}`),
      ),
      `a JWT from the ${shape} import must verify against the same public key`,
    );
  }
});

Deno.test("the JWT says what GitHub requires, and backdates iat", async () => {
  const { pkcs8 } = await keyPair();
  const now = 1_700_000_000_000;
  const jwt = await appJwt("123456", await importAppKey(pkcs8), now);
  const [head, body] = jwt.split(".").slice(0, 2)
    .map((p) => JSON.parse(new TextDecoder().decode(unb64url(p))));

  assertEquals(head, { alg: "RS256", typ: "JWT" });
  assertEquals(body.iss, "123456");
  // Backdated, because GitHub refuses a token issued in its own future and a
  // host clock a few seconds fast is the ordinary way that happens.
  assertEquals(body.iat, now / 1000 - 60);
  // And inside GitHub's ten-minute ceiling, measured from the real issue time
  // rather than from the backdated one.
  assert(body.exp - now / 1000 <= 10 * 60, `exp is ${body.exp - now / 1000}s away`);
  assert(body.exp > now / 1000);
});
