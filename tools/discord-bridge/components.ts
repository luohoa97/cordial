/**
 * Components V2, which is all-or-nothing.
 *
 * Setting `IS_COMPONENTS_V2` opts a message *out* of the classic shape
 * entirely: `content` and `embeds` are refused outright --
 *
 *     MESSAGE_CANNOT_USE_LEGACY_FIELDS_WITH_COMPONENTS_V2
 *     The 'content' field cannot be used when using MessageFlags.IS_COMPONENTS_V2
 *
 * -- and every piece of text has to become a Text Display component. The flag
 * was set alongside `content` here for a while, which nothing noticed until
 * the thread controls became the first components the bridge sent; every
 * thread then failed to open while the issue was filed anyway.
 *
 * It is worth adopting rather than avoiding. A Container gives the accent bar
 * an embed gave, plus separators and real ordering, and it drops the embed's
 * field limits -- so the forms message and the issue threads are laid out
 * rather than crammed into a description.
 */

export const IS_COMPONENTS_V2 = 1 << 15;

export const TEXT_DISPLAY = 10;
export const SEPARATOR = 14;
export const CONTAINER = 17;

/** Markdown, as a block of text. The V2 replacement for `content`. */
export function text(content: string) {
  return { type: TEXT_DISPLAY, content };
}

/** A rule between blocks. `divider` draws the line; without it it is just space. */
export function separator(divider = true, spacing: 1 | 2 = 1) {
  return { type: SEPARATOR, divider, spacing };
}

/**
 * A bordered block with a coloured edge -- what an embed's left bar used to be.
 */
export function container(accent: number, children: unknown[]) {
  return { type: CONTAINER, accent_color: accent, components: children };
}

/** The message body for a V2 message: components only, and the flag. */
export function v2(components: unknown[]) {
  return { components, flags: IS_COMPONENTS_V2 };
}
