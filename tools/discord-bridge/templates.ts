/**
 * Where the forms come from at runtime, and what happens when they break.
 *
 * The templates are fetched from GitHub rather than baked in, so editing
 * `.github/ISSUE_TEMPLATE/` changes the Discord forms without a redeploy.
 * That is the point, and it moves one failure: a template edited into a shape
 * the modal cannot hold now fails **in front of a user** instead of in CI.
 *
 * So this keeps the last set that parsed. A bad edit leaves the bridge serving
 * the previous forms and saying loudly that it did, which is strictly better
 * than a button that opens nothing -- and `deno task check` still runs the same
 * parse in CI, so the normal way to find out remains "before shipping it".
 */
import { FormError, type IssueForm, parseForm } from "./issue_forms.ts";

const API = "https://api.github.com";

export interface Source {
  owner: string;
  repo: string;
  ref: string;
  token?: string;
}

interface Cached {
  forms: IssueForm[];
  fetchedAt: number;
  /** Why the newest attempt was rejected, if these are being served stale. */
  stale?: string;
}

/** Long enough that a busy channel costs one fetch, short enough to matter. */
const TTL_MS = 5 * 60 * 1000;

export class Templates {
  #source: Source;
  #cache: Cached | null = null;
  #now: () => number;
  #read: (path: string) => Promise<string>;

  constructor(
    source: Source,
    options: { now?: () => number; read?: (path: string) => Promise<string> } = {},
  ) {
    this.#source = source;
    this.#now = options.now ?? Date.now;
    // Injected so the tests can drive the whole path -- parse, reject, fall
    // back -- without a network, which is the only way the fallback gets
    // exercised at all.
    this.#read = options.read ?? ((path) => this.#fetchFile(path));
  }

  async #fetchFile(path: string): Promise<string> {
    const { owner, repo, ref, token } = this.#source;
    const url = `${API}/repos/${owner}/${repo}/contents/${path}?ref=${ref}`;
    const headers: Record<string, string> = {
      "accept": "application/vnd.github.raw+json",
      "user-agent": "cordial-issue-bridge",
    };
    if (token) headers.authorization = `Bearer ${token}`;
    const response = await fetch(url, { headers });
    if (!response.ok) {
      throw new FormError(`${path}: GitHub answered ${response.status}`);
    }
    return await response.text();
  }

  async #listTemplates(): Promise<string[]> {
    const { owner, repo, ref, token } = this.#source;
    const url = `${API}/repos/${owner}/${repo}/contents/.github/ISSUE_TEMPLATE?ref=${ref}`;
    const headers: Record<string, string> = {
      "accept": "application/vnd.github+json",
      "user-agent": "cordial-issue-bridge",
    };
    if (token) headers.authorization = `Bearer ${token}`;
    const response = await fetch(url, { headers });
    if (!response.ok) {
      throw new FormError(`listing templates: GitHub answered ${response.status}`);
    }
    const entries = await response.json() as { name: string; type: string }[];
    return entries
      .filter((e) => e.type === "file" && e.name.endsWith(".yml") && e.name !== "config.yml")
      .map((e) => e.name)
      .sort();
  }

  /**
   * The current forms, refetched at most every {@link TTL_MS}.
   *
   * Never throws once a good set has been seen: a later bad set is reported
   * through {@link stale} and the good one keeps being served.
   */
  async forms(names?: string[]): Promise<IssueForm[]> {
    if (this.#cache && this.#now() - this.#cache.fetchedAt < TTL_MS) {
      return this.#cache.forms;
    }
    try {
      const files = names ?? await this.#listTemplates();
      const forms: IssueForm[] = [];
      for (const name of files) {
        const text = await this.#read(`.github/ISSUE_TEMPLATE/${name}`);
        forms.push(parseForm(name.replace(/\.ya?ml$/, ""), text));
      }
      if (!forms.length) throw new FormError("no issue forms found");
      // **The short-description drift guard is deliberately not run here.** It
      // compares the hand-written phrasings against every field in every
      // template, so it is only meaningful over the whole set -- and this path
      // may legitimately see a subset if GitHub's directory listing hiccups or
      // a template is briefly absent. Failing the fetch for that would serve
      // stale forms, or on a cold start refuse to serve any, for something
      // that is not a runtime fault: an unused phrasing hurts nobody. `deno
      // task check` runs it over the full set in CI, which is where it belongs.
      this.#cache = { forms, fetchedAt: this.#now() };
      return forms;
    } catch (error) {
      const why = error instanceof Error ? error.message : String(error);
      if (this.#cache) {
        // Do not advance `fetchedAt`: a transient failure should be retried on
        // the next interaction rather than pinned for the whole TTL.
        this.#cache.stale = why;
        console.error(`templates: serving the last good set -- ${why}`);
        return this.#cache.forms;
      }
      throw error;
    }
  }

  /** Why the served forms are behind the repository, if they are. */
  get stale(): string | undefined {
    return this.#cache?.stale;
  }
}
