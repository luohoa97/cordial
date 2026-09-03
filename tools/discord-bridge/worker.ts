/**
 * Run the bridge on Cloudflare Workers.
 *
 * A Worker gets its configuration as the fetch handler's second argument, not
 * from a global, and it is handed a *fresh* argument per request while the
 * module scope persists between them. So the handler is built once and cached:
 * building it mints nothing and touches no network, but `importAppKey` does
 * real cryptographic work and there is no reason to repeat it per request.
 *
 * The cache is keyed on nothing, deliberately -- a Worker isolate serves one
 * environment for its whole life, so a second `env` cannot arrive.
 */
import { build, ConfigError, type Env } from "./main.ts";

/** The slice of `ExecutionContext` the bridge uses. */
interface Ctx {
  waitUntil(promise: Promise<unknown>): void;
}

let handler:
  | ((request: Request, waitUntil?: (p: Promise<unknown>) => void) => Promise<Response>)
  | null = null;

export default {
  async fetch(request: Request, env: Env, ctx: Ctx): Promise<Response> {
    try {
      handler ??= await build(env);
    } catch (error) {
      // A misconfigured Worker must say so in a way somebody can read, rather
      // than throwing an opaque 1101. This is the one place the bridge answers
      // without having been configured.
      const why = error instanceof ConfigError ? error.message : String(error);
      console.error(`configuration: ${why}`);
      return new Response(`the bridge is not configured: ${why}\n`, { status: 503 });
    }
    // **`ctx.waitUntil` is not optional here.** Without it, everything the
    // bridge does after acknowledging an interaction -- filing the issue,
    // opening the thread, editing the reply -- is cancelled the moment this
    // function returns, and the user is left looking at "is thinking" for ever.
    return await handler(request, ctx.waitUntil.bind(ctx));
  },
};
