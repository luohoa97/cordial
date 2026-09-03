/**
 * Where the issue forms live on disk, independent of the working directory.
 *
 * `deno task` runs from this directory and a developer runs `deno test` from
 * the repository root, so a relative path is right in one place and wrong in
 * the other -- and the failure is a permission error about a path nobody
 * typed, which reads as a sandbox problem rather than a `cd`.
 */
export const TEMPLATE_DIR = new URL("../../.github/ISSUE_TEMPLATE", import.meta.url).pathname;
