/**
 * The GitHub side: authenticate as the App, file the issue, relay the comment.
 *
 * A GitHub App rather than a personal token, because the issues are filed on
 * behalf of people who are not the maintainer. An App has its own identity in
 * the tracker, its installation can be scoped to this one repository, and
 * revoking it does not mean rotating somebody's account credentials.
 *
 * ## The private key comes in two shapes
 *
 * GitHub hands out **PKCS#1** (`BEGIN RSA PRIVATE KEY`) and Web Crypto imports
 * only **PKCS#8** (`BEGIN PRIVATE KEY`). Rather than make that the operator's
 * problem -- an `openssl` incantation in a deployment guide that half of
 * people get wrong once and never think about again -- the PKCS#1 body is
 * wrapped into PKCS#8 here, which is a fixed ASN.1 prefix and nothing more.
 * Both shapes are accepted and the tests cover both.
 */

const API = "https://api.github.com";
const UA = "cordial-issue-bridge";

/**
 * `SEQUENCE { INTEGER 0, SEQUENCE { OID rsaEncryption, NULL }, OCTET STRING [...] }`
 * with the lengths left to be filled in. This is the whole of the difference
 * between the two encodings for an RSA key.
 */
function pkcs1ToPkcs8(pkcs1: Uint8Array): Uint8Array<ArrayBuffer> {
  const algorithm = [
    0x30,
    0x0d,
    0x06,
    0x09,
    0x2a,
    0x86,
    0x48,
    0x86,
    0xf7,
    0x0d,
    0x01,
    0x01,
    0x01,
    0x05,
    0x00,
  ];
  const derLength = (n: number): number[] => {
    if (n < 0x80) return [n];
    const bytes: number[] = [];
    for (let v = n; v > 0; v >>>= 8) bytes.unshift(v & 0xff);
    return [0x80 | bytes.length, ...bytes];
  };
  const octet = [0x04, ...derLength(pkcs1.length), ...pkcs1];
  const inner = [0x02, 0x01, 0x00, ...algorithm, ...octet];
  const der = [0x30, ...derLength(inner.length), ...inner];
  return new Uint8Array(new ArrayBuffer(der.length)).map((_, i) => der[i]);
}

function pemBody(pem: string): { der: Uint8Array<ArrayBuffer>; pkcs1: boolean } {
  // **A PEM in an environment variable almost always has escaped newlines.**
  // It is multi-line and env vars are not, so every hosting provider's answer
  // is `KEY="-----BEGIN...\n...-----END..."` with a literal backslash and n.
  // Stripping whitespace does not touch those: the backslash goes but the `n`
  // is a valid base64 character and stays, so it lands *inside* the body and
  // `atob` fails with "Failed to decode base64" -- an error naming neither the
  // key nor the escaping.
  //
  // Found the hard way. The bridge's first real deployment crashed at startup
  // on exactly this and served nothing, with no runtime log to say why,
  // because the isolate died before it could write one.
  const text = pem.replace(/\\r/g, "").replace(/\\n/g, "\n");
  const pkcs1 = text.includes("BEGIN RSA PRIVATE KEY");
  const base64 = text.replace(/-----[^-]+-----/g, "").replace(/\s+/g, "");
  const binary = atob(base64);
  const der = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i++) der[i] = binary.charCodeAt(i);
  return { der, pkcs1 };
}

export async function importAppKey(pem: string): Promise<CryptoKey> {
  const { der, pkcs1 } = pemBody(pem);
  return await crypto.subtle.importKey(
    "pkcs8",
    pkcs1 ? pkcs1ToPkcs8(der) : der,
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["sign"],
  );
}

function base64url(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/**
 * A short-lived App JWT.
 *
 * `iat` is backdated a minute because GitHub rejects a token whose issue time
 * is in its future, and a host clock a few seconds fast is the common way that
 * happens. Ten minutes is the maximum GitHub accepts.
 */
export async function appJwt(appId: string, key: CryptoKey, now = Date.now()): Promise<string> {
  const seconds = Math.floor(now / 1000);
  const encode = (o: unknown) => base64url(new TextEncoder().encode(JSON.stringify(o)));
  const head = encode({ alg: "RS256", typ: "JWT" });
  const body = encode({ iat: seconds - 60, exp: seconds + 9 * 60, iss: appId });
  const signature = await crypto.subtle.sign(
    "RSASSA-PKCS1-v1_5",
    key,
    new TextEncoder().encode(`${head}.${body}`),
  );
  return `${head}.${body}.${base64url(new Uint8Array(signature))}`;
}

export interface Repo {
  owner: string;
  repo: string;
}

export class GitHub {
  #repo: Repo;
  #token: () => Promise<string>;

  constructor(repo: Repo, token: () => Promise<string>) {
    this.#repo = repo;
    this.#token = token;
  }

  async #call(method: string, path: string, body?: unknown): Promise<Response> {
    const response = await fetch(`${API}${path}`, {
      method,
      headers: {
        "accept": "application/vnd.github+json",
        "authorization": `Bearer ${await this.#token()}`,
        "content-type": "application/json",
        "user-agent": UA,
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!response.ok) {
      throw new Error(
        `${method} ${path}: GitHub answered ${response.status} ${await response.text()}`,
      );
    }
    return response;
  }

  async createIssue(
    title: string,
    body: string,
    labels: string[],
  ): Promise<{ number: number; html_url: string }> {
    const { owner, repo } = this.#repo;
    const response = await this.#call("POST", `/repos/${owner}/${repo}/issues`, {
      title,
      body,
      labels,
    });
    return await response.json();
  }

  /**
   * Replace the body, which is how the thread id gets in.
   *
   * The issue has to exist before its thread does -- the thread's opening
   * message quotes the issue number -- so the marker cannot be written at
   * creation. One extra call, and it keeps the pairing in the artefact rather
   * than in a table.
   */
  async setIssueBody(number: number, body: string): Promise<void> {
    const { owner, repo } = this.#repo;
    await this.#call("PATCH", `/repos/${owner}/${repo}/issues/${number}`, { body });
  }

  async comment(number: number, body: string): Promise<void> {
    const { owner, repo } = this.#repo;
    await this.#call("POST", `/repos/${owner}/${repo}/issues/${number}/comments`, {
      body,
    });
  }
}

/** Exchange an App JWT for the installation token the REST calls use. */
export async function installationToken(
  jwt: string,
  installationId: string,
): Promise<{ token: string; expires_at: string }> {
  const response = await fetch(
    `${API}/app/installations/${installationId}/access_tokens`,
    {
      method: "POST",
      headers: {
        "accept": "application/vnd.github+json",
        "authorization": `Bearer ${jwt}`,
        "user-agent": UA,
      },
    },
  );
  if (!response.ok) {
    throw new Error(`installation token: GitHub answered ${response.status}`);
  }
  return await response.json();
}
