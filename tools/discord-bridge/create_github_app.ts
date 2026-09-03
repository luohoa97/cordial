#!/usr/bin/env -S deno run --allow-net --allow-read --allow-write --allow-env
/**
 * Create the bridge's GitHub App through the manifest flow.
 *
 * The alternative is `github.com/settings/apps/new`, which is a long form with
 * two dozen permission dropdowns. Getting one of them wrong produces an App
 * that authenticates and then cannot file an issue, and the error says
 * "Resource not accessible by integration" without naming the permission. The
 * manifest flow states the whole configuration up front, so there is nothing
 * to mis-tick: GitHub shows it, a person approves it, and what comes back is
 * exactly what was asked for.
 *
 * It also hands back the **private key and the webhook secret** in the
 * conversion response, which is the other reason to prefer it -- otherwise the
 * key is a browser download and the secret is invented by hand.
 *
 * ## How the secrets are handled
 *
 * They go from the response straight to disk: the key to a `0600` `.pem`, the
 * rest appended to `.env`. Nothing is printed, nothing is logged, and nothing
 * is returned to the caller. The only things this reports are the App's id,
 * its slug and its URL, none of which are secret.
 */

const PORT = 8479;
const REDIRECT = `http://localhost:${PORT}/callback`;
const HERE = new URL(".", import.meta.url).pathname;

const owner = Deno.env.get("GITHUB_OWNER") ?? "luohoa97";
const repo = Deno.env.get("GITHUB_REPO") ?? "cordial";

/**
 * `active: false` on the webhook, deliberately.
 *
 * There is no public URL until the bridge is deployed, and GitHub would
 * otherwise start delivering to a placeholder and marking the deliveries
 * failed. The secret is still generated and still recorded, so turning the
 * webhook on later is one field in the App's settings and no rotation.
 */
const manifest = {
  name: `Cordial Issues (${owner})`,
  url: `https://github.com/${owner}/${repo}`,
  hook_attributes: { url: "https://example.invalid/github", active: false },
  redirect_url: REDIRECT,
  public: false,
  // Exactly what the bridge does and nothing else: open issues, comment on
  // them, and edit the body once to write the thread marker in. It never reads
  // code, never touches pull requests, and has no business with either.
  default_permissions: { issues: "write", metadata: "read" },
  default_events: ["issue_comment"],
  description:
    "Files Cordial issues from Discord, so people without a GitHub account can report properly.",
};

const state = crypto.randomUUID();

function landing(): Response {
  // A self-submitting form, because the manifest flow is a POST and a link
  // cannot carry a body. Nothing here is secret; the approval happens on
  // GitHub's own page.
  return new Response(
    `<!doctype html><meta charset="utf-8"><title>Create the Cordial Issues GitHub App</title>
<body style="font:16px/1.5 system-ui;max-width:34rem;margin:4rem auto;padding:0 1rem">
<h1>Creating the GitHub App…</h1>
<p>GitHub will show you exactly what it is about to create. Approve it there.</p>
<form id="f" action="https://github.com/settings/apps/new?state=${state}" method="post">
  <input type="hidden" name="manifest" id="m">
  <noscript><button type="submit">Continue to GitHub</button></noscript>
</form>
<script>
document.getElementById("m").value = ${JSON.stringify(JSON.stringify(manifest))};
document.getElementById("f").submit();
</script>`,
    { headers: { "content-type": "text/html; charset=utf-8" } },
  );
}

async function convert(code: string): Promise<Response> {
  const response = await fetch(
    `https://api.github.com/app-manifests/${code}/conversions`,
    { method: "POST", headers: { accept: "application/vnd.github+json", "user-agent": "cordial" } },
  );
  if (!response.ok) {
    return new Response(`GitHub answered ${response.status}: ${await response.text()}`, {
      status: 500,
    });
  }
  const app = await response.json();

  // 0600 at creation, so the key is never briefly world-readable.
  const pem = `${HERE}github-app.pem`;
  const file = await Deno.open(pem, { write: true, create: true, truncate: true, mode: 0o600 });
  await file.write(new TextEncoder().encode(app.pem));
  file.close();
  await Deno.chmod(pem, 0o600);

  const env = `${HERE}github-app.env`;
  const envFile = await Deno.open(env, { write: true, create: true, truncate: true, mode: 0o600 });
  await envFile.write(new TextEncoder().encode(
    [
      `GITHUB_APP_ID=${app.id}`,
      `GITHUB_APP_LOGIN=${app.slug}`,
      `GITHUB_WEBHOOK_SECRET=${app.webhook_secret}`,
      `GITHUB_APP_PRIVATE_KEY_PATH=${pem}`,
      "",
    ].join("\n"),
  ));
  envFile.close();
  await Deno.chmod(env, 0o600);

  console.log(`\n  created: ${app.name}`);
  console.log(`  app id:  ${app.id}`);
  console.log(`  slug:    ${app.slug}   (this is GITHUB_APP_LOGIN)`);
  console.log(`  key:     ${pem} (0600)`);
  console.log(`  env:     ${env} (0600, holds the webhook secret)`);
  console.log(`\n  install it: ${app.html_url}/installations/new`);

  setTimeout(() => Deno.exit(0), 500);
  return new Response(
    `<!doctype html><meta charset="utf-8"><body style="font:16px/1.5 system-ui;max-width:34rem;margin:4rem auto;padding:0 1rem">
<h1>Created</h1><p><b>${app.name}</b> — app id ${app.id}, slug <code>${app.slug}</code>.</p>
<p>The private key and webhook secret were written to disk with mode 0600. They were not displayed.</p>
<p>Next: <a href="${app.html_url}/installations/new">install it on the repository</a>.</p>`,
    { headers: { "content-type": "text/html; charset=utf-8" } },
  );
}

Deno.serve(
  { port: PORT, onListen: () => console.log(`open http://localhost:${PORT}/`) },
  (request) => {
    const url = new URL(request.url);
    if (url.pathname === "/callback") {
      if (url.searchParams.get("state") !== state) {
        return new Response("state mismatch — start again", { status: 400 });
      }
      const code = url.searchParams.get("code");
      if (!code) return new Response("no code in the callback", { status: 400 });
      return convert(code);
    }
    return landing();
  },
);
