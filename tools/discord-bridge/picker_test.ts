import { TEMPLATE_DIR } from "./repo.ts";
import { assert, assertEquals, assertStringIncludes } from "jsr:@std/assert@^1.0.8";
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

Deno.test("every form gets a button, and Discord's five-to-a-row limit is respected", () => {
  const message = pickerMessage(forms, "https://github.com/o/r") as { components: Row[] };
  for (const row of message.components) {
    assert(row.components.length <= 5, `a row has ${row.components.length} buttons`);
  }
  const ids = message.components.flatMap((r) => r.components.map((c) => c.custom_id))
    .filter(Boolean);
  assertEquals(ids.length, forms.length);
  for (const form of forms) {
    assert(ids.includes(`cordial-issue-open:${form.slug}`), `no button for ${form.slug}`);
  }
});

Deno.test("the route for people who do have an account stays on the message", () => {
  // Somebody with a GitHub account should not be herded through a bot, and the
  // tracker is the same either way.
  const message = pickerMessage(forms, "https://github.com/o/r") as { components: Row[] };
  const link = message.components.at(-1)!.components[0];
  assertEquals(link.url, "https://github.com/o/r/issues/new/choose");
});

Deno.test("the copy leads with the barrier it removes, and names the diagnostics route", () => {
  const message = pickerMessage(forms, "https://github.com/o/r") as {
    embeds: { description: string }[];
  };
  const text = message.embeds[0].description;
  assertStringIncludes(text, "do not need a GitHub account");
  // A required Diagnostics box met cold is where a report gets abandoned.
  assertStringIncludes(text, "cordial --diagnostics");
});
