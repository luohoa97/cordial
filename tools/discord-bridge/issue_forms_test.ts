import { TEMPLATE_DIR } from "./repo.ts";
import { assert, assertEquals, assertStringIncludes, assertThrows } from "jsr:@std/assert@^1.0.8";
import { modalSelections, modalValues } from "./discord.ts";
import {
  checkShortDescriptions,
  droppedFor,
  FormError,
  type IssueForm,
  LABEL,
  labelPickerComponent,
  layoutWithLabels,
  MODAL_MAX_COMPONENTS,
  modalFor,
  parseForm,
  STRING_SELECT,
  TEXT_INPUT,
} from "./issue_forms.ts";

const DIR = TEMPLATE_DIR;

function realForms(): IssueForm[] {
  const forms: IssueForm[] = [];
  for (const entry of [...Deno.readDirSync(DIR)].sort((a, b) => a.name < b.name ? -1 : 1)) {
    if (!entry.isFile || !entry.name.endsWith(".yml") || entry.name === "config.yml") continue;
    forms.push(
      parseForm(entry.name.replace(/\.yml$/, ""), Deno.readTextFileSync(`${DIR}/${entry.name}`)),
    );
  }
  return forms;
}

Deno.test("every real template fits a modal, and keeps all of its required fields", () => {
  const forms = realForms();
  assert(forms.length >= 5, `expected the five issue forms, saw ${forms.length}`);
  for (const form of forms) {
    assert(
      form.placed.length <= MODAL_MAX_COMPONENTS,
      `${form.slug} placed ${form.placed.length}`,
    );
    // The guarantee that matters: nothing required is ever what gives way.
    for (const block of form.fields) {
      if (block.validations?.required) {
        assert(
          form.placed.includes(block),
          `${form.slug}: required field ${block.id} was dropped`,
        );
      }
    }
  }
});

Deno.test("every template still asks for diagnostics, from the Discord side too", () => {
  for (const form of realForms()) {
    const ids = form.placed.map((b) => b.id);
    assert(
      ids.includes("diagnostics"),
      `${form.slug} does not ask for diagnostics in its modal: ${ids.join(", ")}`,
    );
  }
});

Deno.test("a required field that will not fit is an error, not a truncation", () => {
  // The control for the rule above: six required fields cannot be filed
  // silently missing one.
  const yaml = `
name: Too much
body:
${
    ["a", "b", "c", "d", "e", "f"].map((id) =>
      `  - type: textarea
    id: ${id}
    attributes: {label: Field ${id}}
    validations: {required: true}`
    ).join("\n")
  }
`;
  const error = assertThrows(() => parseForm("toomuch", yaml), FormError);
  assertStringIncludes(error.message, "do not fit");
  assertStringIncludes(error.message, "more than 5 required fields");
});

Deno.test("an optional field ahead of a required one is named as the cause", () => {
  const yaml = `
name: Badly ordered
body:
  - type: textarea
    id: chatty
    attributes: {label: Optional and first}
${
    ["a", "b", "c", "d", "e"].map((id) =>
      `  - type: textarea
    id: ${id}
    attributes: {label: Field ${id}}
    validations: {required: true}`
    ).join("\n")
  }
`;
  const error = assertThrows(() => parseForm("misordered", yaml), FormError);
  // The distinction earns its place: this one is fixed by reordering the
  // template, the other by splitting the modal.
  assertStringIncludes(error.message, "chatty");
  assertStringIncludes(error.message, "took slots ahead");
});

Deno.test("a dropdown becomes a String Select, which is what makes it expressible", () => {
  const form = realForms().find((f) => f.slug === "broken_feature")!;
  const modal = modalFor(form) as { components: Record<string, never>[] };
  const kinds = modal.components.map((c) =>
    (c as unknown as { component: { type: number } }).component.type
  );
  assert(kinds.includes(STRING_SELECT), `expected a select among ${kinds.join(", ")}`);
  assert(kinds.includes(TEXT_INPUT));
  for (const component of modal.components) {
    assertEquals((component as unknown as { type: number }).type, LABEL);
  }
});

Deno.test("a stale short description is an error rather than a line nobody notices", () => {
  const forms = realForms();
  checkShortDescriptions(forms); // the real templates must be clean
  const pruned = forms.map((f) => ({
    ...f,
    fields: f.fields.filter((b) => b.id !== "diagnostics"),
  }));
  const error = assertThrows(() => checkShortDescriptions(pruned), FormError);
  assertStringIncludes(error.message, "diagnostics");
});

Deno.test("the follow-up modal carries exactly the fields the first one dropped", () => {
  const form = realForms().find((f) => f.slug === "bug_report")!;
  assert(form.dropped.length > 0, "bug_report is the template with leftovers");
  const extra = modalFor(form, "extra") as { custom_id: string; components: unknown[] };
  assertEquals(extra.components.length, form.dropped.length);
  assertStringIncludes(extra.custom_id, ":extra");
});

// ---- the label picker's slot -------------------------------------------------

const picker = labelPickerComponent(
  [{ name: "area:input", description: "Keyboard and mouse" }, { name: "gpu:amd" }],
  { description: "Optional." },
);

Deno.test("with the picker, every real form still holds all its required fields in five slots", () => {
  for (const form of realForms()) {
    const layout = layoutWithLabels(form);
    assert(layout, `${form.slug} must be able to spare a slot`);
    assert(layout.placed.length <= MODAL_MAX_COMPONENTS - 1, form.slug);
    for (const block of form.fields.filter((b) => b.validations?.required)) {
      assert(layout.placed.includes(block), `${form.slug}: ${block.id} lost its place`);
    }
    // And nothing is dropped without being offered: the follow-up holds five.
    assert(layout.dropped.length <= MODAL_MAX_COMPONENTS, `${form.slug} overflows the follow-up`);
    // Every field is somewhere.
    assertEquals(layout.placed.length + layout.dropped.length, form.fields.length);

    const modal = modalFor(form, "main", picker) as { components: unknown[]; custom_id: string };
    assert(modal.components.length <= MODAL_MAX_COMPONENTS);
    assertEquals(modal.custom_id.endsWith(":main:l"), true);
  }
});

Deno.test("a required field is never pushed out by the picker, even behind an optional one", () => {
  // The shape of `broken_feature`: an optional field ahead of the last required.
  const form = parseForm(
    "t",
    `name: t
body:
  - { type: input, id: a, attributes: { label: A }, validations: { required: true } }
  - { type: input, id: opt, attributes: { label: Opt } }
  - { type: input, id: b, attributes: { label: B }, validations: { required: true } }
  - { type: input, id: c, attributes: { label: C }, validations: { required: true } }
  - { type: input, id: d, attributes: { label: D }, validations: { required: true } }
`,
  );
  // Without a picker the optional one holds its slot (and nothing overflows).
  assertEquals(form.placed.map((b) => b.id), ["a", "opt", "b", "c", "d"]);
  const layout = layoutWithLabels(form)!;
  assertEquals(layout.placed.map((b) => b.id), ["a", "b", "c", "d"]);
  assertEquals(layout.dropped.map((b) => b.id), ["opt"]);
});

Deno.test("a form with five required fields cannot spare a slot, and opens without the picker", () => {
  const form = parseForm(
    "t",
    "name: t\nbody:\n" +
      ["a", "b", "c", "d", "e"].map((id) =>
        `  - { type: input, id: ${id}, attributes: { label: ${id} }, validations: { required: true } }\n`
      ).join(""),
  );
  assertEquals(layoutWithLabels(form), null);
  const modal = modalFor(form, "main", picker) as { components: unknown[]; custom_id: string };
  assertEquals(modal.components.length, 5);
  assertEquals(modal.custom_id, "cordial-issue:t:main", "no `:l`, so nothing expects a picker");
});

Deno.test("the follow-up's field list agrees with whether the picker took a slot", () => {
  const bug = realForms().find((f) => f.slug === "bug_report")!;
  assertEquals(droppedFor(bug, false).length, 4);
  assertEquals(droppedFor(bug, true).length, 5);
});

Deno.test("the picker is optional, multi, and marks what is already chosen", () => {
  const withChosen = labelPickerComponent(
    [{ name: "area:input", description: "Keyboard and mouse" }, { name: "gpu:amd" }],
    { description: "Optional.", selected: ["GPU:AMD"] },
  ) as { type: number; component: Record<string, unknown> };
  assertEquals(withChosen.type, LABEL);
  const select = withChosen.component as {
    type: number;
    required: boolean;
    min_values: number;
    max_values: number;
    options: { value: string; description?: string; default?: boolean }[];
  };
  assertEquals(select.type, STRING_SELECT);
  assertEquals(select.required, false);
  assertEquals(select.min_values, 0);
  assertEquals(select.max_values, 2);
  assertEquals(select.options[0].description, "Keyboard and mouse");
  assertEquals("description" in select.options[1], false, "Discord refuses an empty description");
  assertEquals(select.options.map((o) => o.default ?? false), [false, true]);
});

Deno.test("multi-select answers keep their separate values, commas and all", () => {
  const data = {
    components: [
      { type: 18, component: { custom_id: "title", value: "x" } },
      { type: 18, component: { custom_id: "cordial-labels", values: ["area: a, b", "gpu:amd"] } },
      { type: 18, component: { custom_id: "empty", values: [] } },
    ],
  };
  assertEquals(modalSelections(data), {
    "cordial-labels": ["area: a, b", "gpu:amd"],
    "empty": [],
  });
  // The single-answer reader is unchanged: it joins, which is right for a dropdown.
  assertEquals(modalValues(data)["cordial-labels"], "area: a, b, gpu:amd");
});

Deno.test("the log archive field is the web form's alone, and every bug form asks for it", () => {
  const forms = realForms();
  const asking = forms.filter((f) => f.webOnly.some((b) => b.id === "log-archive"));
  assertEquals(
    asking.map((f) => f.slug).sort(),
    ["broken_feature", "bug_report", "roblox_update"],
    "a form that should ask for the archive stopped, or another started",
  );
  for (const form of forms) {
    // A modal cannot carry a file: the field may be neither in a modal nor in
    // the follow-up.
    const ids = [...form.placed, ...form.dropped].map((b) => b.id);
    assert(!ids.includes("log-archive"), `${form.slug} would show the archive field in Discord`);
    assertEquals(form.webOnly.every((b) => b.validations?.required), true, "and on the web it is required");
  }
});
