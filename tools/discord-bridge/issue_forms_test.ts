import { TEMPLATE_DIR } from "./repo.ts";
import { assert, assertEquals, assertStringIncludes, assertThrows } from "jsr:@std/assert@^1.0.8";
import {
  checkShortDescriptions,
  FormError,
  type IssueForm,
  LABEL,
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
