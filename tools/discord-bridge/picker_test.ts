import { TEMPLATE_DIR } from "./repo.ts";
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@^1.0.8";
import { IS_COMPONENTS_V2 } from "./components.ts";
import { parseForm } from "./issue_forms.ts";
import { pickerMessage } from "./picker.ts";

const forms = [...Deno.readDirSync(TEMPLATE_DIR)]
  .filter((e) => e.isFile && e.name.endsWith(".yml") && e.name !== "config.yml")
  .map((e) =>
    parseForm(e.name.replace(/\.yml$/, ""), Deno.readTextFileSync(`${TEMPLATE_DIR}/${e.name}`))
  );

interface Row {
  type: number;
  components: { label: string; custom_id?: string; url?: string }[];
}

/** The Container's children: text, separators and the button rows. */
function inside(): (Row & { content?: string })[] {
  const message = pickerMessage(forms, "https://github.com/o/r") as {
    flags: number;
    components: { type: number; components: (Row & { content?: string })[] }[];
  };
  assertEquals(message.flags, IS_COMPONENTS_V2, "the picker is a Components V2 message");
  assertEquals(message.components[0].type, 17, "everything sits in one accented Container");
  return message.components[0].components;
}

Deno.test("every form gets a button, and Discord's five-to-a-row limit is respected", () => {
  const rows = inside().filter((c) => c.type === 1);
  for (const row of rows) {
    assert(row.components.length <= 5, `a row has ${row.components.length} buttons`);
  }
  const ids = rows.flatMap((r) => r.components).map((c) => c.custom_id).filter(Boolean);
  assertEquals(ids.length, forms.length);
  for (const form of forms) {
    assert(ids.includes(`cordial-issue-open:${form.slug}`), `no button for ${form.slug}`);
  }
});

Deno.test("the route for people who do have an account stays on the message", () => {
  // Somebody with a GitHub account should not be herded through a bot, and the
  // tracker is the same either way.
  const rows = inside().filter((c) => c.type === 1);
  const link = rows.at(-1)!.components[0];
  assertEquals(link.url, "https://github.com/o/r/issues/new/choose");
});

Deno.test("the copy leads with the barrier it removes, and names the diagnostics route", () => {
  const words = inside().filter((c) => c.type === 10).map((c) => c.content ?? "").join("\n");
  assertStringIncludes(words, "do not need a GitHub account");
  // A required Diagnostics box met cold is where a report gets abandoned.
  assertStringIncludes(words, "cordial --diagnostics");
});

Deno.test("the pitch and the how-to are separated rather than one wall", () => {
  // The reason for moving off an embed: a description is one blob, a Container
  // can put a rule between the two things it is saying.
  const kinds = inside().map((c) => c.type);
  assert(kinds.includes(14), `expected a separator among ${kinds.join(", ")}`);
  assertEquals(kinds.filter((k) => k === 10).length, 2, "two blocks of text, not one");
});
