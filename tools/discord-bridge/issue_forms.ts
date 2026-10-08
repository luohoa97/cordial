/**
 * GitHub issue forms, turned into the Discord modals that mirror them.
 *
 * **Nobody hand-copies a form.** A bot that asks four questions while the web
 * form asks nine files issues a maintainer has to go back and complete, and
 * the two drift the moment somebody edits one and not the other -- which is
 * exactly what happened in this repository between a `PKGBUILD` and the
 * `.SRCINFO` beside it, where the fix for "plugins cannot run" never reached
 * the AUR because the dependency was added to one file and not the other. So
 * `.github/ISSUE_TEMPLATE/` stays the single source and the modals are derived.
 *
 * `config.yml` sets `blank_issues_enabled: false` on purpose, because the
 * required Diagnostics block lives in the forms and a blank issue cannot ask
 * for it. A bridge that posts a chat message as an issue reverses that
 * silently. See ADR-030.
 *
 * ## What Discord allows, checked rather than remembered
 *
 * From the component reference, read 2026-09-03:
 *
 *   - a modal holds **1 to 5 top-level components**, so five fields, since a
 *     `Label` wrapping one input is one top-level component;
 *   - a modal title is at most 45 characters;
 *   - inside a modal every input sits in a `Label` (type 18);
 *   - `Text Input` (4) is modal-only, style 1 short and 2 paragraph;
 *   - `String Select` (3) *is* allowed in a modal, which is the only reason
 *     the two templates with dropdowns are expressible at all.
 *
 * The five-component ceiling is why this is a check and not only a converter:
 * two of the five templates have more fields than that, and which ones give
 * way is a decision that has to be visible.
 */
// npm `yaml` rather than `jsr:@std/yaml`, because a `jsr:` specifier does not
// resolve under Cloudflare Workers' bundler and the bridge has to run on both.
// Deno takes it through the import map below; wrangler takes it from
// node_modules.
import { parse as parseYaml } from "yaml";

export const ACTION_ROW = 1;
export const BUTTON = 2;
export const STRING_SELECT = 3;
export const TEXT_INPUT = 4;
export const LABEL = 18;

export const MODAL_MAX_COMPONENTS = 5;
const MODAL_TITLE_MAX = 45;
/**
 * Discord documents 1-4000 for a text input's `max_length` and 1-100 for a
 * `custom_id`, but states no limit for a `Label` description. This stays well
 * under anything plausible rather than guessing precisely, and a cut is made
 * legible as a cut -- see `clip`.
 */
const DESCRIPTION_MAX = 100;
const PLACEHOLDER_MAX = 100;

/**
 * A web form's `description` is prose with room to breathe; a modal `Label`'s
 * is one short line. Clipping the long one stops mid-word, and for
 * `diagnostics` the half that gets cut is precisely the part saying where to
 * get the block -- the field every template requires.
 *
 * So these are written for the space. **The fields are still derived**; only
 * the phrasing is chosen here, and a key naming a field no template has any
 * more is an error rather than a line nobody notices.
 */
const SHORT_DESCRIPTIONS: Record<string, string> = {
  // **The button, not a command.** This named `cordial --diagnostics`, which
  // does not exist on a Flatpak install -- the largest group of people filing
  // through Discord. Issue #32's reporter ran it, got "command not found", and
  // said so in the field this description was meant to help them fill in. The
  // web template lists the command for all four install types; there is no room
  // for that in 100 characters, and the button works on every one of them.
  "diagnostics": "Main menu → Report a Problem → Copy. That button works on every install.",
  "which-feature":
    'What you tried, and what happened instead. "Nothing happened" is a good answer.',
  "engine-log": "The client's own output, if you have it. Trim it to the interesting part.",
  "runs-attempted": "How many times you tried, if it does not happen every time.",
  "whose-job": "Is this Cordial's to do, a plugin's, or the engine's?",
  "narrowest-effect": "The smallest thing Cordial could do for you, not the channel it would use.",
  "how-you-established-it": "What you ran, and what it printed. Paste the output.",
  "contradicts": "Name the comment, ADR or document this disagrees with, if any.",
  "build-number": "The Roblox version, e.g. 2.736.0.1408.",
  "needs-implementation": "Which symbol, and what the engine does with it.",
};

/**
 * Fields the web form asks for and a Discord modal cannot carry.
 *
 * **`log-archive` asks for a `.zip` to be dragged into the box.** A modal holds
 * text inputs and menus, so the field would be a required box nobody can fill
 * honestly, and it would take a slot from the fields that matter. They are kept
 * out of the modal and the body, and listed in `IssueForm.webOnly` so a test can
 * say a template did not quietly grow another one. The issue says the archive
 * was not attached -- see `renderIssueBody` -- and `Add to the issue` carries a
 * link to one posted in the thread (ADR-030, amended).
 */
export const WEB_ONLY_FIELDS: ReadonlySet<string> = new Set(["log-archive"]);

export interface FormBlock {
  type: string;
  id?: string;
  attributes?: {
    label?: string;
    description?: string;
    placeholder?: string;
    options?: string[];
  };
  validations?: { required?: boolean };
}

export interface IssueForm {
  slug: string;
  name: string;
  labels: string[];
  titlePrefix: string;
  /** Every non-markdown block, in template order -- including the dropped. */
  fields: FormBlock[];
  /** Blocks the web form has and a modal cannot: see [`WEB_ONLY_FIELDS`]. */
  webOnly: FormBlock[];
  /** The five (or fewer) that fit the modal. */
  placed: FormBlock[];
  /** Optional fields that did not fit, offered by a follow-up modal. */
  dropped: FormBlock[];
}

export class FormError extends Error {}

/** The `custom_id` of the label picker, which no template field can share. */
export const LABEL_SELECT_ID = "cordial-labels";

/** Shorten with an ellipsis, so a cut is legible as a cut. */
function clip(text: string | undefined, limit: number): string {
  const flat = (text ?? "").split(/\s+/).filter(Boolean).join(" ");
  return flat.length <= limit ? flat : flat.slice(0, limit - 1).trimEnd() + "…";
}

function isRequired(block: FormBlock): boolean {
  return block.validations?.required === true;
}

/** One issue-form block as one Discord `Label` and its input. */
/**
 * How many characters a modal field of this kind accepts.
 *
 * **Exported because the issue body has to know it too.** Discord's client
 * stops accepting input at `max_length` and says nothing about it, so a person
 * pasting a crash log gets the *first* 4000 characters -- the half without the
 * crash in it. Issue #28 arrived that way: 3996 characters ending mid-word on
 * an `ALooper_addFd` line, with the exit status and the last frames gone, and
 * nothing in the issue saying the log was incomplete. It read as a client that
 * stopped for no reason.
 *
 * `renderIssueBody` compares against this to say so. Two copies of the number
 * would drift, and silently in the direction that matters: raised here and not
 * there, the notice simply stops appearing.
 */
export function maxLengthFor(blockType: string): number {
  return blockType === "input" ? 1000 : 4000;
}

export function fieldToComponent(block: FormBlock): unknown {
  const attrs = block.attributes ?? {};
  const id = block.id;
  if (!id) throw new FormError(`a ${block.type} block has no id`);

  const label: Record<string, unknown> = {
    type: LABEL,
    label: clip(attrs.label ?? id, 45),
  };
  const description = clip(SHORT_DESCRIPTIONS[id] ?? attrs.description, DESCRIPTION_MAX);
  if (description) label.description = description;

  if (block.type === "dropdown") {
    label.component = {
      type: STRING_SELECT,
      custom_id: id,
      required: isRequired(block),
      options: (attrs.options ?? []).map((o) => ({
        label: clip(o, 100),
        value: clip(o, 100),
      })),
    };
  } else if (block.type === "input" || block.type === "textarea") {
    const input: Record<string, unknown> = {
      type: TEXT_INPUT,
      custom_id: id,
      style: block.type === "input" ? 1 : 2,
      required: isRequired(block),
      max_length: maxLengthFor(block.type),
    };
    const placeholder = clip(attrs.placeholder, PLACEHOLDER_MAX);
    if (placeholder) input.placeholder = placeholder;
    label.component = input;
  } else {
    throw new FormError(`no modal component for a ${block.type} block (${id})`);
  }

  return label;
}

/**
 * Fill `capacity` slots in template order; what is left over is dropped if
 * optional and reported if required.
 *
 * Shared between the plain layout and the one that gives a slot to the label
 * picker, so the two cannot disagree about which optional field gives way.
 */
function pack(fields: FormBlock[], capacity: number) {
  const placed: FormBlock[] = [];
  const dropped: FormBlock[] = [];
  const overflowRequired: string[] = [];
  for (const block of fields) {
    if (placed.length < capacity) placed.push(block);
    else if (isRequired(block)) overflowRequired.push(block.id ?? "?");
    else dropped.push(block);
  }
  return { placed, dropped, overflowRequired };
}

/**
 * The layout when the label picker takes one of the five slots.
 *
 * Unlike `pack`, this lets a required field **displace an optional one that
 * came before it**: the picker is the newcomer, and it must not be what costs a
 * form its place. `broken_feature` is the case -- an optional field sits ahead of
 * its last required one, so a first-come layout with four slots would push the
 * required field out. Here the earliest optionals keep the slots left over once
 * every required field has one, and the form's own order is preserved.
 *
 * Null if the required fields alone do not fit in four. The caller then opens
 * the form without a picker, and `deno task check` says so in CI, rather than a
 * template edit quietly costing a form its Diagnostics box.
 */
export function layoutWithLabels(
  form: IssueForm,
): { placed: FormBlock[]; dropped: FormBlock[] } | null {
  const capacity = MODAL_MAX_COMPONENTS - 1;
  const required = form.fields.filter(isRequired).length;
  if (required > capacity) return null;
  let optionalRoom = capacity - required;
  const placed: FormBlock[] = [];
  const dropped: FormBlock[] = [];
  for (const block of form.fields) {
    if (isRequired(block)) placed.push(block);
    else if (optionalRoom > 0) {
      optionalRoom--;
      placed.push(block);
    } else dropped.push(block);
  }
  return { placed, dropped };
}

/**
 * Parse one template and decide what fits.
 *
 * Required fields never give way: filing without Diagnostics is the failure
 * this whole bridge exists to avoid, so a template whose required fields
 * outgrow the modal is an error and not a truncation.
 */
export function parseForm(slug: string, yamlText: string): IssueForm {
  const doc = parseYaml(yamlText) as {
    name?: string;
    labels?: string[];
    title?: string;
    body?: FormBlock[];
  };

  const blocks = (doc.body ?? []).filter((b) => b.type !== "markdown");
  const fields = blocks.filter((b) => !(b.id && WEB_ONLY_FIELDS.has(b.id)));
  const webOnly = blocks.filter((b) => b.id && WEB_ONLY_FIELDS.has(b.id));
  const { placed, dropped, overflowRequired } = pack(fields, MODAL_MAX_COMPONENTS);

  if (overflowRequired.length) {
    // A required field pushed out by an *optional* one that came first is a
    // packing failure, not a template failure. Say which: the fix is to
    // reorder the template in one case and to split the modal in the other.
    const optionalAhead = placed.filter((b) => !isRequired(b)).map((b) => b.id);
    throw new FormError(
      `${slug}: required field(s) ${overflowRequired.join(", ")} do not fit in ` +
        `${MODAL_MAX_COMPONENTS} components` +
        (optionalAhead.length
          ? `; optional field(s) ${optionalAhead.join(", ")} took slots ahead of them`
          : `; the template has more than ${MODAL_MAX_COMPONENTS} required fields ` +
            `and the modal must be split`),
    );
  }

  return {
    slug,
    name: doc.name ?? slug,
    labels: doc.labels ?? [],
    titlePrefix: doc.title ?? "",
    fields,
    webOnly,
    placed,
    dropped,
  };
}

/**
 * The modal Discord opens when somebody presses this form's button.
 *
 * `labelPicker`, when given, is a ready-made `Label` component for the label
 * menu and takes the fifth slot: an optional field of the form moves to the
 * follow-up modal to make room. If the form cannot spare the slot -- a required
 * field would lose its place -- the picker is left out and the form is exactly
 * what it was.
 *
 * For the `extra` part the argument only says whether the main modal carried a
 * picker, so any truthy value will do.
 *
 * Whether the picker was included rides in the `custom_id` (`:l`), because the
 * submit arrives as a fresh interaction and the follow-up button after it needs
 * to know which optional fields moved. There is nowhere else to keep it.
 */
export function modalFor(
  form: IssueForm,
  part: "main" | "extra" = "main",
  labelPicker?: unknown,
): unknown {
  const layout = labelPicker ? layoutWithLabels(form) : null;
  const withPicker = Boolean(labelPicker && layout);
  const source = layout ?? form;
  const blocks = part === "main" ? source.placed : source.dropped.slice(0, MODAL_MAX_COMPONENTS);
  const components = blocks.map(fieldToComponent);
  if (part === "main" && withPicker) components.push(labelPicker);
  return {
    custom_id: `cordial-issue:${form.slug}:${part}${withPicker ? ":l" : ""}`,
    title: clip(part === "main" ? form.name : `More about: ${form.name}`, MODAL_TITLE_MAX),
    components,
  };
}

/**
 * The optional fields a form offers after filing, given whether the picker
 * took a slot. The follow-up button and the extra modal both read this, so the
 * count in the reply and the fields in the dialog cannot disagree.
 */
export function droppedFor(form: IssueForm, withPicker: boolean): FormBlock[] {
  return (withPicker ? layoutWithLabels(form)?.dropped : null) ?? form.dropped;
}

/**
 * One `Label` wrapping a multi-select of GitHub labels.
 *
 * `required: false` and `min_values: 0` because a report must never be blocked
 * on a classification the reporter may not feel qualified to make. `selected`
 * marks options as already chosen, which is how the editor shows what is on
 * the issue now.
 */
export function labelPickerComponent(
  offered: { name: string; description?: string | null }[],
  options: { description: string; selected?: readonly string[] },
): unknown {
  const selected = new Set((options.selected ?? []).map((s) => s.toLowerCase()));
  return {
    type: LABEL,
    label: "Labels",
    description: clip(options.description, DESCRIPTION_MAX),
    component: {
      type: STRING_SELECT,
      custom_id: LABEL_SELECT_ID,
      required: false,
      min_values: 0,
      max_values: Math.max(1, offered.length),
      placeholder: "Optional",
      options: offered.map((l) => {
        const description = clip(l.description ?? "", 100);
        return {
          label: clip(l.name, 100),
          value: l.name,
          ...(description ? { description } : {}),
          ...(selected.has(l.name.toLowerCase()) ? { default: true } : {}),
        };
      }),
    },
  };
}

/**
 * Guard the hand-written phrasings above against a template that moved on.
 *
 * This is the one direction the override table can rot in: a renamed or
 * removed field leaves a key here that silently does nothing, and the modal
 * quietly loses the line telling somebody where to get their diagnostics.
 */
export function checkShortDescriptions(forms: IssueForm[]): void {
  const known = new Set(forms.flatMap((f) => [...f.fields, ...f.webOnly].map((b) => b.id)));
  const stale = Object.keys(SHORT_DESCRIPTIONS).filter((k) => !known.has(k)).sort();
  if (stale.length) {
    throw new FormError(
      `SHORT_DESCRIPTIONS names field(s) no template has any more: ${stale.join(", ")}`,
    );
  }
}
