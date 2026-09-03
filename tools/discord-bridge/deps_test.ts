import { assertEquals } from "jsr:@std/assert@^1.0.8";

/**
 * **Two manifests name the same dependency and they must agree.** Deno
 * resolves `yaml` through `deno.json`'s import map; wrangler resolves it from
 * `node_modules` via `package.json`. Nothing makes them match, and a bridge
 * that parsed templates with one version locally and another in production
 * would be the worst kind of difference to chase -- this repository has
 * already had exactly that shape between a PKGBUILD and its `.SRCINFO`.
 */
Deno.test("the two manifests pin the same yaml", async () => {
  const here = new URL(".", import.meta.url).pathname;
  const deno = JSON.parse(await Deno.readTextFile(`${here}deno.json`));
  const npm = JSON.parse(await Deno.readTextFile(`${here}package.json`));

  const fromMap = deno.imports.yaml; // e.g. "npm:yaml@^2.8.1"
  assertEquals(
    fromMap,
    `npm:yaml@${npm.dependencies.yaml}`,
    "deno.json's import map and package.json must name one version of yaml",
  );
});

/**
 * The Worker entrypoint named in `wrangler.jsonc` has to exist, because a
 * typo there fails at deploy time with a message about a module rather than
 * about the config.
 */
Deno.test("wrangler's entrypoint is a file that exists", async () => {
  const here = new URL(".", import.meta.url).pathname;
  const text = await Deno.readTextFile(`${here}wrangler.jsonc`);
  const main = text.match(/"main":\s*"([^"]+)"/)?.[1];
  assertEquals(typeof main, "string");
  const stat = await Deno.stat(`${here}${main}`);
  assertEquals(stat.isFile, true, `${main} is named in wrangler.jsonc but is not a file`);
});
