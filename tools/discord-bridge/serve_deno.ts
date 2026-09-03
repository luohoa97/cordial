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
  Deno.serve({ port: Number(Deno.env.get("PORT") ?? 8000) }, handler);
} catch (error) {
  if (error instanceof ConfigError) {
    console.error(error.message);
    Deno.exit(2);
  }
  throw error;
}
