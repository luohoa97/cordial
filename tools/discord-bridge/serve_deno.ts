#!/usr/bin/env -S deno run --allow-net --allow-env --allow-read
/**
 * Run the bridge under Deno, for local development.
 *
 * Ten lines, because everything host-specific lives here and nothing else
 * knows which host it is on. The Cloudflare adapter beside this is the same
 * ten lines wearing Workers' shape.
 */
import { build, ConfigError } from "./main.ts";

try {
  const handler = await build(Deno.env.toObject());
  // Wrapped rather than passed straight in: `Deno.serve` calls its handler
  // with `(request, info)`, and `info` would land in the `waitUntil` slot and
  // be called as a function. The type checker caught that; at runtime it would
  // have been a `TypeError` on the first deferred interaction and nowhere
  // else. Deno needs no `waitUntil` anyway -- the process outlives the
  // response, which is the whole difference from a Worker.
  Deno.serve({ port: Number(Deno.env.get("PORT") ?? 8000) }, (request) => handler(request));
} catch (error) {
  if (error instanceof ConfigError) {
    console.error(error.message);
    Deno.exit(2);
  }
  throw error;
}
