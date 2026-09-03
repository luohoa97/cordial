/**
 * Retrying an HTTP call without filing the same issue twice.
 *
 * The bridge had no retry at all: every call threw on the first non-`ok`
 * response, and a deferred interaction that threw left the reporter looking at
 * "Cordial Issues is thinking" for ever. The gap that mattered was **429** --
 * Discord rate-limits and tells you exactly how long to wait, and the old code
 * treated that identically to a permanent failure while discarding the number
 * it was handed.
 *
 * ## Why 429 and a network error are not the same risk
 *
 * A 429 means the request was **refused**, not performed, so repeating it is
 * safe whatever the call does -- there is nothing on the far side to duplicate.
 * A dropped connection is the opposite: the request may have arrived and been
 * committed, and the response lost on the way back. Repeating *that* blindly is
 * how one bug report becomes two issues, which is worse than the failure it
 * was trying to paper over.
 *
 * So the policy is per call site and the default is the cautious one.
 * `createIssue` is **not** idempotent and is never repeated on an unknown
 * outcome; a read, or a 429 anywhere, is.
 */

export interface Policy {
  /**
   * May this be repeated when the outcome is *unknown* -- a dropped
   * connection, a 5xx that might have committed?
   *
   * False for anything that creates something. A 429 is retried regardless,
   * because it is a refusal rather than an unknown.
   */
  idempotent: boolean;
  /** Total attempts, including the first. */
  attempts?: number;
}

/**
 * How long Discord or GitHub asked us to wait, in milliseconds, or null.
 *
 * Discord puts `retry_after` in the JSON body as seconds (and in a header);
 * GitHub uses the standard `Retry-After` header. Both are read because the two
 * clients share this helper.
 */
export async function retryAfterMs(response: Response): Promise<number | null> {
  const header = response.headers.get("retry-after") ??
    response.headers.get("x-ratelimit-reset-after");
  if (header && Number.isFinite(Number(header))) return Number(header) * 1000;
  try {
    const body = await response.clone().json();
    if (typeof body?.retry_after === "number") return body.retry_after * 1000;
  } catch {
    // Not JSON, or already consumed. The caller still gets a sane default.
  }
  return null;
}

/** Longest single wait. Beyond this, failing now beats blocking. */
const MAX_WAIT_MS = 15_000;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Perform a request, retrying where it is safe and honouring `Retry-After`.
 *
 * `make` is called afresh per attempt, because a `Request` body cannot be read
 * twice.
 */
export async function send(
  make: () => Promise<Response>,
  what: string,
  policy: Policy,
): Promise<Response> {
  const attempts = policy.attempts ?? 3;
  let lastError: unknown;

  for (let attempt = 1; attempt <= attempts; attempt++) {
    const last = attempt === attempts;
    let response: Response;
    try {
      response = await make();
    } catch (error) {
      // No response at all. Whether the far side acted on it is unknowable, so
      // only a call that says it is safe gets another go.
      lastError = error;
      if (last || !policy.idempotent) throw error;
      await sleep(Math.min(250 * 2 ** (attempt - 1), MAX_WAIT_MS));
      continue;
    }

    if (response.ok) return response;

    if (response.status === 429) {
      // Refused, not performed: always safe to repeat, and the wait is not a
      // guess -- they told us.
      if (last) return response;
      const wait = Math.min(await retryAfterMs(response) ?? 1000, MAX_WAIT_MS);
      console.warn(`${what}: rate limited, waiting ${wait}ms (attempt ${attempt})`);
      await sleep(wait);
      continue;
    }

    // 5xx may or may not have committed; 4xx will fail again however often it
    // is sent, so repeating one only delays the error.
    const transient = response.status >= 500;
    if (last || !transient || !policy.idempotent) return response;
    console.warn(`${what}: ${response.status}, retrying (attempt ${attempt})`);
    await sleep(Math.min(500 * 2 ** (attempt - 1), MAX_WAIT_MS));
  }

  throw lastError ?? new Error(`${what}: exhausted retries`);
}
