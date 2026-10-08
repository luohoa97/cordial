//! Draw the text editor in the font the engine asked for.
//!
//! The editor is a `gtk::Text` placed on top of a box the engine drew, so the
//! moment it appears the string is re-rendered by a different text stack. Get
//! the family wrong and the characters change shape and weight under the
//! user's cursor -- reported as "the text shifts a lot when you select the
//! text box, like gtk doesnt match the text size as good". Matching the
//! family was never going to be enough either: the *size* the engine hands
//! over is denominated in its own font's metrics, and drawing it verbatim in
//! a different font is why text visibly jumps when a box gains or loses
//! focus -- see [`parse_mappings`]'s note on `fromRbxFontRatio`, which used to
//! say applying it was a mistake and was measured to be the mistake instead.
//!
//! That matters more here than it would elsewhere, because the editor is what
//! the player actually sees. The engine stops drawing the box's own text while
//! it is focused and resumes on blur, so during editing these glyphs are the
//! visible text rather than an overlay on top of it.
//!
//! **Nothing here vendors a Roblox asset and nothing may.** AGENTS.md rules
//! out committing an APK or anything out of one -- and that covers the id
//! table as much as the font files, which is why there is no id-to-family
//! literal anywhere below. This reads both from the APK the *user* supplied,
//! which is the same posture as every other asset: Cordial ships none and
//! works from the copy on the machine. Extracted files land in the cache
//! beside the extracted libraries, and are rewritten only when their size
//! differs, so a launch does not rewrite them every time.
//!
//! ## Where the font id comes from, and what is still unknown
//!
//! **The engine names the font.** `com.roblox.engine.jni.model.NativeTextBoxInfo`
//! -- the styling spec handed to `showKeyboard` -- declares an `int` field
//! spelled exactly `font`, read out of `classes2.dex` on 2026-08-27.
//! `assets/android/fonts/font-mappings.json` in the same APK maps that integer
//! to a font file. So a per-box font needs no hand-maintained table that would
//! rot across Roblox builds; the authoritative one is already on disk.
//!
//! **Which constructor slot carries the id is now confirmed, as of
//! 2026-08-30.** This used to say slot 6 fit the two-box capture (`0` on
//! both) exactly as well as `Enum.Font.Legacy=0`, because nothing here could
//! tell "the font id, always Legacy" from "some other field, always zero"
//! apart. That ambiguity is resolved from outside this codebase: mocktail
//! (`~/Projects/mocktail/src/jnivm/jnivm.cc:4016-4024`, Apache-2.0) implements
//! the same `NativeTextBoxInfo` constructor and lists its six int arguments in
//! declared order as `xAlignment, yAlignment, textColor, font, textInputType,
//! returnKeyType` -- a fact about Roblox's platform API, taken and credited
//! rather than copied, per AGENTS.md's line between the idea and the
//! transcription. Slot 6 is `xAlignment`, not `font`; `font` is slot 9,
//! exactly where [`font_slot`]'s default already pointed it. `slot_value`
//! keeps reading positionally rather than by field name -- see that
//! function's own doc comment for why -- but the slot it defaults to is now
//! corroborated instead of merely chosen under ambiguity.
//!
//! Reading `TextBox.FontFace` out of the DataModel would answer it directly and
//! is permanently out of scope: that is in-process introspection of the engine,
//! which ADR-001 and ADR-003 rule out. Disassembling the constructor's `iput`
//! order would also answer it, and is declined on the licence line in
//! AGENTS.md -- declared shapes and call order are observation, the body of a
//! method is how it implements something.
//!
//! ## Two corrections to what was written down before this module was widened
//!
//! **The shipped table is not gapless and does not cover every `Enum.Font`.**
//! `docs/NEXT.md` and this file both called it "a 48-entry table ... in a
//! gapless run". 48 entries is right and the run 44-51 is gapless, but the ids
//! are `1, 2, 6..51`: **0, 3, 4 and 5 have no row at all**, and 3-5 are
//! `SourceSans`, `SourceSansBold` and `SourceSansLight`, which is the engine's
//! own default face for an unstyled TextLabel. `SourceSansPro-Regular.ttf`
//! ships in the same archive and the table simply does not point at it.
//! Roblox's own Android mapping not covering Roblox's own default font is the
//! strongest available evidence that a fallback is mandatory rather than a
//! nicety -- their code must have one too. An id with no row falls back to
//! the default family and says so once.
//!
//! **Ids 3, 4 and 5 are now answered anyway, and this reverses the earlier
//! refusal to write them down.** That paragraph argued a literal
//! `3 => "SourceSansPro-Regular"` was a hand-maintained table of Roblox
//! knowledge. It is, and leaving the gap cost something concrete: a box set to
//! `SourceSans` drew in Builder Sans, a different typeface, while `SourceSansItalic`
//! (6) next to it drew in Source Sans Pro, so restyling a box between the two
//! changed typeface as well as slant. [`SOURCE_SANS_FALLBACKS`] is the whole
//! table -- three ids from Roblox's public `Enum.Font` documentation and three
//! file names from the archive's own listing -- and the family, weight and
//! slant are still read out of the staged file by fontconfig and checked
//! against what the id means. A file that disagrees is not used.
//!
//! **Slant is allowed only to ids that mean it.** Id 6 is the one italic
//! member of the enum and `SourceSansPro-It.ttf` is the one italic file the
//! manifest points at; [`ITALIC_IDS`] refuses a slanted face for any other id,
//! so a mispairing can degrade a box to the upright default but cannot slant
//! one. Ids with no face of their own -- 0, anything past 51, `Unknown` -- keep
//! drawing in the default: upright, weight 400.
//!
//! **`assets/content/fonts/families/*.json` is not the source for the family
//! string.** This file used to say it "gives the family string Pango wants",
//! and that is right for `BuilderSans.json` and wrong in general. Pango asks
//! fontconfig, and fontconfig reads the font file, not Roblox's manifest. The
//! two disagree: `LegacyArimo.json` and `LegacyArial.json` both declare
//! `"name": "Arimo (Legacy)"` while both pointing at `Arimo-Regular.ttf`,
//! which fontconfig reads as family `Arimo` -- so inverting the manifests is
//! not even a function, one file carries three different declared names. Their
//! weights disagree too: `ComicNeue-Angular-Bold.ttf` is declared as the
//! `Regular` face at weight 400 and fontconfig reads it as weight 700.
//! `FcFreeTypeQuery` on the extracted file is therefore the source used here,
//! because it is by construction the answer Pango will resolve against.
//!
//! ## Cost, and why the whole set is registered at once but not at launch
//!
//! Measured on this host against the user's own APK, 46 distinct files:
//! `FcConfigAppFontAddFile` accepted 46 of 46 in 48.8 ms and `FcFreeTypeQuery`
//! read family, weight and slant from all 46 in 10.7 ms. So registering
//! everything is cheap in absolute terms, but it also means extracting about
//! 7.2 MB out of the archive -- on every launch, for a feature that only
//! matters once a game restyles a box.
//!
//! The compromise is that [`install`] still does exactly one font at window
//! creation, so a launch costs what it costs today, and the full set is built
//! on the first font lookup instead -- which happens when a TextBox is first
//! focused, long after startup. The whole set goes in together rather than one
//! file per id so that a family is never handed to Pango before its file is in
//! fontconfig, which is the ordering the existing single-font path already
//! relies on.
//!
//! `FcConfigAppFontAddFile` is reached through `dlsym` rather than linked.
//! fontconfig is already in the process -- GTK cannot render without it -- and
//! dlsym keeps it from becoming a build-time dependency of this crate for one
//! call. The same pattern `vulkan.rs` uses for the loader.

use std::collections::{BTreeMap, HashSet};
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// What fontconfig calls Roblox's UI font, and therefore what Pango wants.
/// Confirmed with `fc-query` against the shipped file rather than assumed from
/// the filename: the file is `BuilderSans-Regular.otf`, the family is
/// `Builder Sans`.
pub const FAMILY: &str = "Builder Sans";

const ASSET: &str = "content/fonts/BuilderSans-Regular.otf";
const MAPPINGS: &str = "android/fonts/font-mappings.json";
const FONT_ASSET_DIR: &str = "content/fonts/";

/// The `Enum.Font` ids whose manifest row is genuinely a slanted face.
///
/// Roblox documents the numbering publicly
/// (create.roblox.com/docs/reference/engine/enums/Font): `SourceSansItalic` is
/// 6 and no other member is italic. An id outside this list that resolves to
/// a slanted file is therefore a wrong answer -- a renumbered manifest, or a
/// family match landing on the one italic file in a family -- and is refused
/// rather than drawn, because "upright regular when unsure" is the failure a
/// reader of the text can ignore and slanted text in a chat box is not.
const ITALIC_IDS: [i32; 1] = [6];

/// Ids the APK's manifest has no row for, although each is a plain Source
/// Sans Pro face the archive ships, as `(id, file, OpenType weight)`.
///
/// `SourceSans` (3), `SourceSansBold` (4) and `SourceSansLight` (5) are the
/// engine's default family for an unstyled box and the manifest skips them.
/// Left unanswered they drew in Builder Sans, which is a different typeface,
/// while the neighbouring ids 6 and 16 drew in Source Sans Pro -- so a game
/// that restyled one box from Source Sans to Source Sans Italic changed
/// typeface as well as slant. The ids come from Roblox's public `Font` enum
/// and the file names from the archive's own `content/fonts/` listing; the
/// family, weight and slant are still read out of the staged file by
/// fontconfig, and the weight here is only what the result is checked
/// against. A file that reads as anything else is not used, and the id keeps
/// drawing in the default face rather than in a face nobody checked.
const SOURCE_SANS_FALLBACKS: [(i32, &str, i32); 3] = [
    (3, "SourceSansPro-Regular.ttf", 400),
    (4, "SourceSansPro-Bold.ttf", 700),
    (5, "SourceSansPro-Light.ttf", 300),
];

/// The id whose manifest row names the Source Sans family, borrowed for its
/// size ratio by the ids in [`SOURCE_SANS_FALLBACKS`]. `INFERRED`: the
/// manifest gives 6 and 16 different ratios, so the number is a closest guess
/// from the unhinted-weight sibling and not a measurement of ids 3-5.
const SOURCE_SANS_RATIO_ID: i32 = 6;

/// A face the editor can ask Pango for: everything needed to name one shipped
/// font file unambiguously.
///
/// Family alone is not enough and that is not a hypothetical. The 46 files the
/// mapping references collapse to 38 fontconfig families -- four files are
/// `Builder Sans`, four are `Montserrat`, two `Source Sans Pro`, two `Arimo` --
/// so ids 46, 47, 48 and 49 all name the same family and differ only in weight.
/// Setting family alone would draw Roblox's Medium, Bold and ExtraBold boxes in
/// Regular.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    /// The family string as fontconfig reads it out of the file.
    pub family: String,
    /// OpenType/CSS weight -- 400 regular, 700 bold. `FcWeightToOpenType`
    /// converts fontconfig's own scale (80, 200, 210) into this one, because
    /// Pango's `Weight` is in OpenType units and confusing the two would ask
    /// for weight 80, which is Thin.
    pub weight: i32,
    pub italic: bool,
    /// The manifest's `fromRbxFontRatio` for this id, or `1.0` when nothing
    /// closer to the truth is known -- see [`parse_mappings`] for what this
    /// corrects and why it was not applied for a long time.
    pub from_rbx_font_ratio: f32,
}

impl Face {
    /// The face to draw with when nothing better is known: Roblox's UI font at
    /// regular weight. Used before the table is built, when the archive cannot
    /// be read, and for every id that has no row.
    ///
    /// `from_rbx_font_ratio` is `1.0` here deliberately: this face names no id,
    /// so there is no row to read a ratio from, and inventing one would be a
    /// second guess stacked on the first rather than a correction.
    fn default_face() -> Face {
        Face { family: FAMILY.to_owned(), weight: 400, italic: false, from_rbx_font_ratio: 1.0 }
    }
}

/// Register Roblox's UI font with the process, returning the family name for
/// the editor to ask for.
///
/// `None` on any failure, and every caller must treat that as "use whatever
/// Pango would have used". A missing font is a cosmetic mismatch; refusing to
/// draw an editor over it would make typing invisible, which is the bug this
/// whole path exists to fix.
pub fn install() -> Option<&'static str> {
    let path = stage(ASSET)?;
    if !register(&path) {
        return None;
    }
    DEFAULT_REGISTERED.store(true, std::sync::atomic::Ordering::Release);
    println!("[android] editor font: registered {FAMILY} from the APK");
    Some(FAMILY)
}

/// Whether [`install`] actually got [`FAMILY`] into fontconfig.
///
/// Read by [`default_face`], which must not name a family Pango cannot resolve.
/// Asking for an unregistered family draws *something* -- Pango substitutes
/// silently -- so a face built on the constant when the constant was never
/// registered is a value that looks resolved and is not, which is the same
/// class of mistake as a stub returning success.
static DEFAULT_REGISTERED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The face for a font id the engine named, or `None` when the id has no row in
/// the APK's own table.
///
/// A `None` is a real answer and not a failure to look: it means this build
/// ships no file for that id, which is what a marketplace font looks like from
/// here -- see the note on [`log_unresolved`]. The caller draws the default
/// face and says which id it could not resolve, rather than drawing nothing.
pub fn face_for_id(id: i32) -> Option<&'static Face> {
    table().by_id.get(&id)
}

/// The face to use for an id that resolved to nothing, or `None` when there is
/// no face this process can honestly claim.
///
/// Prefers the row the archive gives for the font the editor already draws with
/// -- so if a future build renumbers or reweights Builder Sans, this follows.
/// Falls back to the constant only when [`install`] actually registered it, and
/// otherwise gives `None`, which puts the caller back on the process-wide
/// family exactly as it behaved before per-box fonts existed.
pub fn default_face() -> Option<&'static Face> {
    table().default.as_ref()
}

/// Every candidate slot, in the order the trace prints them.
///
/// Five, not six: slot 8 is `textColor`, pinned by a packed ARGB value nothing
/// else in the class could be, and slots 0-4 are the floats. Slot 10 is on the
/// list even though `wayland.rs` already reads it as `textInputType` for the
/// password mask, because that reading is itself `INFERRED` from the same
/// single login capture and this is the experiment that would disprove it.
pub const CANDIDATE_SLOTS: [u8; 5] = [6, 7, 9, 10, 11];

/// The slot `native/android_classes.cpp` guesses, and this module's default.
///
/// **`INFERRED`, and weakly.** It rests on one capture of two Login-screen
/// boxes in which slot 9 read 46 on both and never varied, and 46 is the id the
/// shipped table gives for `BuilderSans-Regular.otf`, which is what the login
/// screen visibly draws. That is consistent but it is not discriminating: slot
/// 6 read 0 on both, `Enum.Font.Legacy` is 0, and a constant is consistent with
/// any field that happens not to vary between two boxes on one screen.
const DEFAULT_FONT_SLOT: u8 = 9;

/// Which constructor slot to read the font id out of.
///
/// `CORDIAL_TEXTBOX_FONT_SLOT=<6|7|9|10|11>` overrides it, and
/// `CORDIAL_TEXTBOX_FONT_SLOT=none` turns per-box fonts off entirely so the
/// editor draws the default family the way it did before this existed. The
/// switch is here because the slot is genuinely unknown and settling it takes a
/// person in a game that restyled a box: with it, a wrong default is one
/// variable away from right and the control -- the same session with the
/// feature off -- costs nothing. Without it, every attempt is a rebuild.
///
/// Rejected values fall back loudly rather than silently, on the same reasoning
/// as `CORDIAL_WHEEL_SCALE`: a quietly ignored slot reads as "per-box fonts
/// still do not work".
pub fn font_slot() -> Option<u8> {
    static SLOT: OnceLock<Option<u8>> = OnceLock::new();
    *SLOT.get_or_init(|| {
        let raw = std::env::var("CORDIAL_TEXTBOX_FONT_SLOT").ok();
        match parse_font_slot(raw.as_deref()) {
            Ok(slot) => slot,
            Err(bad) => {
                eprintln!(
                    "[cordial] CORDIAL_TEXTBOX_FONT_SLOT={bad} is not one of \
                     {CANDIDATE_SLOTS:?} or \"none\"; using slot {DEFAULT_FONT_SLOT}"
                );
                Some(DEFAULT_FONT_SLOT)
            }
        }
    })
}

/// `Ok(Some(slot))` for a candidate, `Ok(None)` for "off", `Err(the input)` for
/// anything else. Split out from [`font_slot`] because the `OnceLock` and the
/// environment make that one untestable and this one pure.
fn parse_font_slot(raw: Option<&str>) -> Result<Option<u8>, String> {
    let Some(raw) = raw else {
        return Ok(Some(DEFAULT_FONT_SLOT));
    };
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("none") || trimmed.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    match trimmed.parse::<u8>() {
        Ok(slot) if CANDIDATE_SLOTS.contains(&slot) => Ok(Some(slot)),
        _ => Err(raw.to_owned()),
    }
}

/// The value of one constructor slot, by number.
///
/// Deliberately a match on the slot number rather than an index into the
/// struct: `RawTextBoxInfo` mixes floats and ints, the float slots have no font
/// id in them under any reading, and a numeric index would happily hand back
/// slot 3 reinterpreted as an integer. Slots that cannot carry an id return
/// `None` rather than a plausible number.
pub fn slot_value(
    info: &cordial_linker_sys::game_activity::RawTextBoxInfo,
    slot: u8,
) -> Option<i32> {
    match slot {
        6 => Some(info.x_alignment),
        7 => Some(info.y_alignment),
        9 => Some(info.font),
        10 => Some(info.text_input_type),
        11 => Some(info.return_key_type),
        _ => None,
    }
}

/// The font id this spec carries, under whichever slot is currently selected,
/// or `None` when per-box fonts are switched off.
pub fn font_id(info: &cordial_linker_sys::game_activity::RawTextBoxInfo) -> Option<i32> {
    slot_value(info, font_slot()?)
}

/// Say once, per distinct id, that an id could not be resolved.
///
/// **Once, because this is called on every keystroke.** The editor is re-styled
/// each time the overlay is rebuilt, so a per-call line would bury the log of a
/// chat session in one repeated sentence.
///
/// And it is said at all because the alternative is the lie this project's
/// rules are about. An unresolvable id is not noise: the shipped table stops at
/// 51 and `Enum.Font.Unknown` is 100, which is the value a GUI object's legacy
/// `Font` takes when its `FontFace` has no enum member -- a marketplace or
/// custom font. Cordial cannot draw that one through this channel at all (the
/// class declares no string field and a Java `int` cannot hold an asset id), so
/// the number in this line is the whole of what a bug report has to go on.
pub fn log_unresolved(id: i32, drawn: Option<&Face>) {
    static SEEN: OnceLock<Mutex<HashSet<i32>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut seen = seen.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.insert(id) {
        return;
    }
    match drawn {
        Some(face) => eprintln!(
            "[cordial] editor font: id {id} has no row in the APK's font-mappings.json; \
             drawing {} instead",
            face.family
        ),
        None => eprintln!(
            "[cordial] editor font: id {id} has no row in the APK's font-mappings.json \
             and no shipped face could be registered; the editor keeps whatever family \
             the process already had"
        ),
    }
}

// ------------------------------------------------------------- the table

struct Table {
    by_id: BTreeMap<i32, Face>,
    /// `None` when nothing this process can name was registered -- see
    /// [`default_face`].
    default: Option<Face>,
}

/// The table that means "the archive told us nothing", which is not the same as
/// "this build ships no fonts" and must not draw as though it were.
fn empty_table() -> Table {
    Table { by_id: BTreeMap::new(), default: registered_default() }
}

/// The constant face, but only if it is really in fontconfig.
fn registered_default() -> Option<Face> {
    DEFAULT_REGISTERED
        .load(std::sync::atomic::Ordering::Acquire)
        .then(Face::default_face)
}

/// Build the id-to-face table once, on first use.
///
/// Everything that can go wrong here ends in a table that is empty or short
/// rather than a failed launch, because none of it is worth refusing to draw an
/// editor over. A font that would not register is *absent* from the table
/// rather than present with a guessed family -- a row claiming a family Pango
/// cannot resolve is a stub that lies, and the editor would silently draw
/// something else while the log said it had found the right one.
fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(build_table)
}

fn build_table() -> Table {
    let started = std::time::Instant::now();
    let Some(bytes) = super::asset::read_asset(MAPPINGS) else {
        eprintln!(
            "[android] editor font: assets/{MAPPINGS} is not in this APK; \
             every box keeps the process-wide family"
        );
        return empty_table();
    };
    let entries = match parse_mappings(bytes) {
        Ok(entries) => entries,
        Err(why) => {
            eprintln!(
                "[android] editor font: assets/{MAPPINGS} did not parse ({why}); \
                 every box keeps the process-wide family"
            );
            return empty_table();
        }
    };

    // Stage and register each distinct file once, and remember the failures as
    // well as the successes. Two ids share a file in this build --
    // `Arimo-Regular.ttf` is both 1 and 50 -- so without the `None` entries a
    // file that would not register is retried per id and counted twice, and the
    // summary line below then reports more unusable files than there are.
    let mut faces: BTreeMap<String, Option<Face>> = BTreeMap::new();
    let load = |file: &str| -> Option<Face> {
        stage(&format!("{FONT_ASSET_DIR}{file}"))
            .filter(|path| register(path))
            .and_then(|path| query_face(&path))
    };
    for (_, file, _) in &entries {
        if !faces.contains_key(file) {
            let face = load(file);
            faces.insert(file.clone(), face);
        }
    }
    let mut by_id = rows_to_faces(&entries, &faces);
    add_missing_source_sans(&mut by_id, |file| {
        // Same per-file cache as the rows, so a build that does add a row for
        // one of these files does not stage it twice.
        if !faces.contains_key(file) {
            let face = load(file);
            faces.insert(file.to_owned(), face);
        }
        faces.get(file).cloned().flatten()
    });
    let failed = faces.values().filter(|f| f.is_none()).count();

    // The default follows the archive where the archive has an opinion. If a
    // future build renumbers Builder Sans or ships it at a different weight,
    // this tracks it; the constant is only reached when the table is unusable.
    let default = pick_default(&by_id);

    println!(
        "[android] editor font: {} ids resolved ({} manifest rows) to {} faces in {} ms{}",
        by_id.len(),
        entries.len(),
        faces.len() - failed,
        started.elapsed().as_millis(),
        if failed == 0 { String::new() } else { format!(", {failed} file(s) unusable") }
    );
    Table { by_id, default }
}

/// Turn the manifest's rows and what fontconfig read out of each file into the
/// id-to-face table.
///
/// A row whose file would not register is absent, not guessed. And a row that
/// resolves to a slanted file for an id that is not one of [`ITALIC_IDS`] is
/// dropped with a line saying so: `Face::italic` is read from the file, and a
/// family with a single italic member is exactly where a wrong id-to-file
/// pairing would draw slanted text with nothing in the log to explain it.
fn rows_to_faces(
    entries: &[(i32, String, f32)],
    faces: &BTreeMap<String, Option<Face>>,
) -> BTreeMap<i32, Face> {
    let mut by_id = BTreeMap::new();
    for (id, file, ratio) in entries {
        // The file cache is keyed on filename, because staging and registering
        // is the expensive, per-*file* step and two ids can share a file. The
        // ratio is a per-*id* row, so it is stamped onto this id's own copy
        // rather than folded into the cache, which would silently give a
        // second id the first one's number.
        let Some(Some(face)) = faces.get(file) else { continue };
        if !admit(*id, face) {
            eprintln!(
                "[android] editor font: id {id} maps to {file}, which is italic, and id {id} \
                 is not an italic font; drawing the default face for it instead"
            );
            continue;
        }
        let mut face = face.clone();
        face.from_rbx_font_ratio = *ratio;
        by_id.insert(*id, face);
    }
    by_id
}

/// Whether `face` may stand for `id`: slanted faces only for italic ids.
fn admit(id: i32, face: &Face) -> bool {
    !face.italic || ITALIC_IDS.contains(&id)
}

/// Give ids 3, 4 and 5 their Source Sans Pro face, where the manifest has no
/// row for them and the archive ships the file. See [`SOURCE_SANS_FALLBACKS`].
///
/// A row the archive does supply always wins, so a future build that adds
/// these ids is followed rather than overridden. `load` stages, registers and
/// reads one file, and is a parameter so the choice of face can be tested
/// without fontconfig.
fn add_missing_source_sans(
    by_id: &mut BTreeMap<i32, Face>,
    mut load: impl FnMut(&str) -> Option<Face>,
) {
    let ratio = by_id.get(&SOURCE_SANS_RATIO_ID).map_or(1.0, |f| f.from_rbx_font_ratio);
    for (id, file, weight) in SOURCE_SANS_FALLBACKS {
        if by_id.contains_key(&id) {
            continue;
        }
        let Some(mut face) = load(file) else { continue };
        // Read from the file and then checked against the enum's meaning, in
        // both directions: a file that reads as slanted or at another weight
        // is not what this id names, and a wrong face drawn confidently is the
        // stub that lies.
        if face.italic || face.weight != weight {
            eprintln!(
                "[android] editor font: {file} reads as weight {} italic {}, not the upright \
                 weight {weight} that id {id} names; id {id} keeps the default face",
                face.weight, face.italic
            );
            continue;
        }
        face.from_rbx_font_ratio = ratio;
        by_id.insert(id, face);
    }
}

/// The face an id with no face of its own is drawn in: upright, regular
/// weight, in the engine's UI family. Never a neighbour's face -- a family match
/// may pick a file, but not for an id it was not asked about, which is how a
/// slanted face could reach an upright request.
fn pick_default(by_id: &BTreeMap<i32, Face>) -> Option<Face> {
    by_id
        .values()
        .find(|f| f.family == FAMILY && f.weight == 400 && !f.italic)
        .cloned()
        .or_else(registered_default)
}

/// Read `assets/android/fonts/font-mappings.json` into id-to-filename pairs.
///
/// Parsed as a `serde_json::Value` rather than through derived structs because
/// this crate does not carry `serde`'s derive feature and one manifest does not
/// justify adding it.
///
/// **`fromRbxFontRatio` is now applied, and it took a measurement to get
/// there.** This used to say the ratio was deliberately ignored: every row
/// carries one, a fraction below or equal to one, and it plainly exists to
/// reconcile Roblox's text sizing with Android's per font, but "the size the
/// editor draws at today is right" was a belief and not an observation --
/// nobody had put the two renderings side by side. Focusing the Home search
/// bar and measuring both, 2026-08-30: the engine's own unfocused "Search"
/// draws an 11px-tall capital against the GTK editor's 15px-tall capital on
/// "Size" in the same box, a ~1.36x jump. `font-mappings.json`'s row for that
/// box's font (id 46, Builder Sans) gives `fromRbxFontRatio` as
/// 0.7936507937 -- almost exactly 1/1.26, the reciprocal of the jump actually
/// measured. That is the multiplier this file was refusing to apply, on the
/// strength of an observation that turns out to have been looking at the
/// wrong box. `host_window.rs` multiplies `font_size` by it before handing
/// Pango an absolute size; see that file for why the size is absolute at all.
fn parse_mappings(bytes: &[u8]) -> Result<Vec<(i32, String, f32)>, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let array = value.as_array().ok_or("top level is not an array")?;
    let mut out = Vec::with_capacity(array.len());
    for entry in array {
        let Some(id) = entry.get("enum").and_then(serde_json::Value::as_i64) else {
            continue;
        };
        let Some(file) = entry.get("font").and_then(serde_json::Value::as_str) else {
            continue;
        };
        // The manifest's `font` is a bare filename relative to
        // `assets/content/fonts/`, and it is joined onto both an asset name and
        // a cache path below. A row carrying a separator or a `..` would walk
        // out of both, so it is dropped rather than sanitised -- this is a
        // signed archive and a row that shape means the assumption has changed,
        // which is worth noticing rather than papering over.
        if file.is_empty() || file.contains('/') || file.contains('\\') || file.contains("..") {
            continue;
        }
        let Ok(id) = i32::try_from(id) else { continue };
        // A row missing the ratio, or carrying one that is not a plain
        // number, still names a usable font -- the ratio only refines the
        // size, and 1.0 is "draw it the size the engine said" exactly as
        // every id behaved before this field existed.
        let ratio = entry
            .get("fromRbxFontRatio")
            .and_then(serde_json::Value::as_f64)
            .map(|r| r as f32)
            .unwrap_or(1.0);
        out.push((id, file.to_owned(), ratio));
    }
    if out.is_empty() {
        return Err("no usable rows".into());
    }
    Ok(out)
}

// ------------------------------------------------------- fontconfig, by dlsym

/// Extract one asset to the font cache and return where it landed.
fn stage(asset: &str) -> Option<PathBuf> {
    let bytes = super::asset::read_asset(asset)?;
    let name = asset.rsplit('/').next()?;
    let path = cache_dir()?.join(name);
    std::fs::create_dir_all(path.parent()?).ok()?;
    // Size is a weak comparison and deliberately so: this is a versioned
    // asset out of a signed APK, not something a user edits, and hashing
    // every font on every launch to catch a same-size replacement is not worth
    // the startup cost.
    let stale = std::fs::metadata(&path).map(|m| m.len() != bytes.len() as u64).unwrap_or(true);
    if stale {
        std::fs::write(&path, bytes).ok()?;
    }
    Some(path)
}

/// Hand one file to the process's fontconfig. `false` if it would not take it.
fn register(path: &std::path::Path) -> bool {
    let Some(add) = fc_sym::<fc::AddFile>("FcConfigAppFontAddFile") else {
        return false;
    };
    let Some(c_path) = path.to_str().and_then(|p| CString::new(p).ok()) else {
        return false;
    };
    // SAFETY: the signature matches fontconfig's public declaration, the path
    // is NUL-terminated and outlives the call, and a null config means "the
    // current configuration", which is what GTK built.
    unsafe { add(std::ptr::null_mut(), c_path.as_ptr()) != 0 }
}

/// What fontconfig reads out of a file: the family Pango will match on, the
/// weight in OpenType units, and whether it is slanted.
///
/// Asking fontconfig rather than trusting the filename is the whole point.
/// `HWYGOTH.ttf` is family `Highway Gothic` and `zekton_rg.ttf` is `Zekton`;
/// neither could have been guessed, and `Comic Neue Angular` is a bold file the
/// families manifest calls Regular.
fn query_face(path: &std::path::Path) -> Option<Face> {
    let query = fc_sym::<fc::Query>("FcFreeTypeQuery")?;
    let get_string = fc_sym::<fc::GetString>("FcPatternGetString")?;
    let get_integer = fc_sym::<fc::GetInteger>("FcPatternGetInteger")?;
    let destroy = fc_sym::<fc::Destroy>("FcPatternDestroy")?;
    let to_opentype = fc_sym::<fc::WeightToOpenType>("FcWeightToOpenType")?;

    let c_path = path.to_str().and_then(|p| CString::new(p).ok())?;
    let mut count: c_int = 0;
    // SAFETY: the path outlives the call; a null `blanks` is what fontconfig's
    // own callers pass; `count` is a live out-parameter. The returned pattern
    // is owned here and destroyed below on every path out.
    let pattern = unsafe { query(c_path.as_ptr(), 0, std::ptr::null_mut(), &mut count) };
    if pattern.is_null() {
        return None;
    }
    let mut family: *mut c_char = std::ptr::null_mut();
    let mut weight: c_int = -1;
    let mut slant: c_int = 0;
    // SAFETY: object names are NUL-terminated literals and every out-parameter
    // is live. `FcResultMatch` is 0. The string belongs to the pattern, so it
    // is copied before the pattern is destroyed.
    let face = unsafe {
        let ok_family = get_string(pattern, c"family".as_ptr(), 0, &mut family) == 0;
        let ok_weight = get_integer(pattern, c"weight".as_ptr(), 0, &mut weight) == 0;
        let _ = get_integer(pattern, c"slant".as_ptr(), 0, &mut slant);
        let family = if ok_family && !family.is_null() {
            CStr::from_ptr(family).to_string_lossy().into_owned()
        } else {
            String::new()
        };
        let out = if family.is_empty() {
            None
        } else {
            Some(Face {
                // fontconfig's own weight scale is not Pango's: Regular is 80
                // there and 400 here, Bold 200 and 700. Handing Pango an 80
                // would ask for Thin, which is a real face in several of these
                // families and would draw.
                weight: if ok_weight { to_opentype(weight) } else { 400 },
                // FC_SLANT_ROMAN is 0; italic is 100 and oblique 110.
                italic: slant != 0,
                family,
                // This is the per-*file* cache keyed on filename (`build_table`
                // below), and the ratio is a per-*id* manifest row -- two ids
                // can share a file. `1.0` here is overwritten with the real
                // value for every id that names this file; it is never the
                // number an id actually gets drawn with.
                from_rbx_font_ratio: 1.0,
            })
        };
        destroy(pattern);
        out
    };
    face
}

/// fontconfig's entry points, resolved out of whatever already loaded it.
mod fc {
    use super::{c_char, c_int, c_uint, c_void};

    pub type AddFile = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
    pub type Query =
        unsafe extern "C" fn(*const c_char, c_uint, *mut c_void, *mut c_int) -> *mut c_void;
    pub type GetString =
        unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_char) -> c_int;
    pub type GetInteger =
        unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_int) -> c_int;
    pub type Destroy = unsafe extern "C" fn(*mut c_void);
    pub type WeightToOpenType = unsafe extern "C" fn(c_int) -> c_int;
}

/// One fontconfig symbol out of the process, or `None` if fontconfig is not in
/// it -- which is handled rather than assumed away, because a headless unit
/// test process has no GTK in it and must not abort here.
fn fc_sym<T: Copy>(symbol: &str) -> Option<T> {
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    let name = CString::new(symbol).ok()?;
    // SAFETY: RTLD_DEFAULT is NULL on glibc and the name is NUL-terminated.
    let sym = unsafe { dlsym(std::ptr::null_mut(), name.as_ptr()) };
    if sym.is_null() {
        return None;
    }
    // SAFETY: every `T` this is instantiated with is a function pointer type
    // declared to match fontconfig's public header, checked for size above.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&sym) })
}

/// Beside the extracted libraries, for the same reason they are there: it came
/// out of the APK and is reproducible from it, so it belongs in a cache the
/// user can delete rather than in their data directory.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("cordial").join("fonts"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordial_linker_sys::game_activity::RawTextBoxInfo;

    /// The shape the shipped manifest actually has, reduced to three rows.
    /// Written by hand rather than copied: what is under test is the parser,
    /// and a fixture lifted out of the APK would be exactly the vendored
    /// Roblox material AGENTS.md forbids.
    const SAMPLE: &str = r#"[
        { "enum": 1, "font": "One-Regular.ttf", "fromRbxFontRatio": 0.895 },
        { "enum": 46, "font": "Two-Regular.otf", "fromRbxFontRatio": 0.5 },
        { "enum": 50, "font": "One-Regular.ttf", "fromRbxFontRatio": 0.895 }
    ]"#;

    #[test]
    fn mappings_parse_to_id_filename_and_ratio() {
        let rows = parse_mappings(SAMPLE.as_bytes()).expect("sample parses");
        assert_eq!(
            rows,
            vec![
                (1, "One-Regular.ttf".to_owned(), 0.895),
                (46, "Two-Regular.otf".to_owned(), 0.5),
                (50, "One-Regular.ttf".to_owned(), 0.895),
            ]
        );
    }

    /// A row with no `fromRbxFontRatio` at all -- most of the shipped table,
    /// historically, before this field was read -- still names a usable font.
    /// `1.0` is "draw it the size the engine said", which is every id's
    /// behaviour before this field existed and must stay correct when the
    /// manifest simply says nothing.
    #[test]
    fn a_missing_ratio_defaults_to_one() {
        let json = r#"[{ "enum": 1, "font": "NoRatio-Regular.ttf" }]"#;
        assert_eq!(parse_mappings(json.as_bytes()).unwrap(), vec![(1, "NoRatio-Regular.ttf".to_owned(), 1.0)]);
    }

    /// Two ids sharing one file is not a defect: this build maps 1 and 50 both
    /// to `Arimo-Regular.ttf`, and the builder must register it once and give
    /// both ids a row.
    #[test]
    fn one_file_may_serve_two_ids() {
        let rows = parse_mappings(SAMPLE.as_bytes()).unwrap();
        let distinct: std::collections::BTreeSet<_> = rows.iter().map(|(_, f, _)| f).collect();
        assert_eq!(rows.len(), 3);
        assert_eq!(distinct.len(), 2);
    }

    /// A row that could walk out of `assets/content/fonts/` is dropped, not
    /// sanitised. Both the asset name and the cache path are built by joining
    /// this string.
    #[test]
    fn rows_that_escape_the_font_directory_are_dropped() {
        let json = r#"[
            { "enum": 1, "font": "../../../etc/passwd" },
            { "enum": 2, "font": "sub/dir/Font.ttf" },
            { "enum": 3, "font": "" },
            { "enum": 4, "font": "Good-Regular.ttf" }
        ]"#;
        let rows = parse_mappings(json.as_bytes()).unwrap();
        assert_eq!(rows, vec![(4, "Good-Regular.ttf".to_owned(), 1.0)]);
    }

    /// A row missing either key is skipped rather than failing the file: one
    /// bad row in a future build must not cost every other font.
    #[test]
    fn incomplete_rows_are_skipped_and_the_rest_survive() {
        let json = r#"[
            { "enum": 1 },
            { "font": "Orphan.ttf" },
            { "enum": "seven", "font": "Stringy.ttf" },
            { "enum": 9, "font": "Kept.ttf" }
        ]"#;
        assert_eq!(parse_mappings(json.as_bytes()).unwrap(), vec![(9, "Kept.ttf".to_owned(), 1.0)]);
    }

    /// Malformed or empty input is an error, so the caller logs it and draws
    /// the default rather than proceeding on an empty table it mistook for a
    /// build that ships no fonts.
    #[test]
    fn unusable_manifests_are_errors_not_empty_tables() {
        assert!(parse_mappings(b"not json at all").is_err());
        assert!(parse_mappings(b"{}").is_err());
        assert!(parse_mappings(b"[]").is_err());
        assert!(parse_mappings(br#"[{"enum":1}]"#).is_err());
    }

    #[test]
    fn the_default_slot_is_the_one_the_native_side_guesses() {
        assert_eq!(parse_font_slot(None), Ok(Some(DEFAULT_FONT_SLOT)));
        assert_eq!(DEFAULT_FONT_SLOT, 9);
    }

    #[test]
    fn every_candidate_slot_is_selectable() {
        for slot in CANDIDATE_SLOTS {
            assert_eq!(parse_font_slot(Some(&slot.to_string())), Ok(Some(slot)));
        }
        assert_eq!(parse_font_slot(Some(" 11 ")), Ok(Some(11)));
    }

    /// "none" is the control the experiment needs: the same session with
    /// per-box fonts off, without a rebuild.
    #[test]
    fn the_feature_can_be_switched_off() {
        assert_eq!(parse_font_slot(Some("none")), Ok(None));
        assert_eq!(parse_font_slot(Some("NONE")), Ok(None));
        assert_eq!(parse_font_slot(Some("off")), Ok(None));
    }

    /// Slot 8 is `textColor` and slots 0-4 are floats. Naming one of them is a
    /// mistake worth a message, not a silent reinterpretation of a colour as a
    /// font id.
    #[test]
    fn slots_that_cannot_hold_a_font_id_are_rejected() {
        for bad in ["8", "0", "4", "5", "12", "14", "99", "-1", "", "nine"] {
            assert_eq!(parse_font_slot(Some(bad)), Err(bad.to_owned()), "input {bad:?}");
        }
    }

    /// The one thing that must not drift: the slot number the switch names has
    /// to select the field of that number in the mirror.
    #[test]
    fn slot_numbers_select_the_field_of_that_number() {
        let info = RawTextBoxInfo {
            x_alignment: 606,
            y_alignment: 707,
            text_color: 808,
            font: 909,
            text_input_type: 1010,
            return_key_type: 1111,
            ..Default::default()
        };
        assert_eq!(slot_value(&info, 6), Some(606));
        assert_eq!(slot_value(&info, 7), Some(707));
        assert_eq!(slot_value(&info, 9), Some(909));
        assert_eq!(slot_value(&info, 10), Some(1010));
        assert_eq!(slot_value(&info, 11), Some(1111));
        // Not 808: reading textColor as a font id is the mistake this guards.
        assert_eq!(slot_value(&info, 8), None);
        for other in [0u8, 1, 2, 3, 4, 5, 12, 13, 14, 200] {
            assert_eq!(slot_value(&info, other), None, "slot {other}");
        }
    }

    /// **The FFI driven rather than asserted.** `register` and `query_face`
    /// are hand-written signatures against fontconfig's headers reached by
    /// `dlsym`, so a transposed argument is a segfault or a plausible-looking
    /// wrong family, never a compile error.
    ///
    /// It cannot use a shipped Roblox font: those live in an APK this
    /// repository does not have and must not have. Any font on the host does
    /// the job, because what is under test is the call shape and not the file.
    ///
    /// The failure path is asserted unconditionally -- a missing file must give
    /// a clean `false`/`None` and not a crash -- and the success path only
    /// where there is a fontconfig and a font to feed it. That is a real gap on
    /// a bare machine, and it says so rather than passing quietly.
    #[test]
    fn the_fontconfig_calls_are_the_shapes_fontconfig_declares() {
        let missing = std::path::Path::new("/nonexistent/definitely-not-a-font.ttf");
        assert!(!register(missing), "a path that is not there cannot register");
        assert_eq!(query_face(missing), None, "and cannot be queried");

        // RTLD_GLOBAL so the later `dlsym(RTLD_DEFAULT, ...)` in `fc_sym` can
        // see the symbols; a unit-test process has no GTK in it to have loaded
        // fontconfig already, which is the whole difference from the client.
        extern "C" {
            fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
        }
        const RTLD_NOW: c_int = 2;
        const RTLD_GLOBAL: c_int = 0x100;
        let name = CString::new("libfontconfig.so.1").unwrap();
        // SAFETY: a NUL-terminated literal and glibc's own flag values.
        let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        if handle.is_null() {
            println!("no libfontconfig on this host; the success path went untested");
            return;
        }
        let Some(font) = any_host_font() else {
            println!("no font found under /usr/share/fonts; the success path went untested");
            return;
        };
        assert!(register(&font), "fontconfig would not take {}", font.display());
        let face = query_face(&font).unwrap_or_else(|| panic!("no face from {}", font.display()));
        assert!(!face.family.is_empty(), "a face with no family is not a face");
        // OpenType weights, not fontconfig's 0-215 scale. Catching the two
        // being confused is most of why this test exists: 80 is Regular there
        // and Thin here.
        assert!(
            (1..=1000).contains(&face.weight),
            "{} reported weight {}, which is not an OpenType weight",
            font.display(),
            face.weight
        );
        println!(
            "{} -> family {:?} weight {} italic {}",
            font.display(),
            face.family,
            face.weight,
            face.italic
        );
    }

    /// The first font file under the host's font directory, at any depth.
    fn any_host_font() -> Option<PathBuf> {
        fn walk(dir: &std::path::Path, depth: u32) -> Option<PathBuf> {
            if depth == 0 {
                return None;
            }
            let mut dirs = Vec::new();
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("ttf") | Some("otf")
                ) {
                    return Some(path);
                }
            }
            dirs.sort();
            dirs.iter().find_map(|d| walk(d, depth - 1))
        }
        walk(std::path::Path::new("/usr/share/fonts"), 6)
    }

    // ------------------------------------------- which id draws which face
    //
    // Faces here are written by hand, in the shape fontconfig reads them out of
    // the shipped Source Sans Pro files (checked with `fc-scan` on the user's
    // own APK: Regular 400 upright, Bold 700, Light 300, Italic 400 slanted,
    // Semibold 600). What is under test is which id gets which of them.

    fn face(family: &str, weight: i32, italic: bool) -> Face {
        Face { family: family.to_owned(), weight, italic, from_rbx_font_ratio: 1.0 }
    }

    /// Source Sans Pro as fontconfig reads the five shipped files.
    fn source_sans(file: &str) -> Option<Face> {
        let (weight, italic) = match file {
            "SourceSansPro-Regular.ttf" => (400, false),
            "SourceSansPro-Bold.ttf" => (700, false),
            "SourceSansPro-Light.ttf" => (300, false),
            "SourceSansPro-It.ttf" => (400, true),
            "SourceSansPro-Semibold.ttf" => (600, false),
            _ => return None,
        };
        Some(face("Source Sans Pro", weight, italic))
    }

    /// The table as the shipped manifest shapes it: 6 and 16 present, 3-5 not,
    /// Builder Sans at 46.
    fn shipped_shape() -> BTreeMap<i32, Face> {
        let entries: Vec<(i32, String, f32)> = vec![
            (6, "SourceSansPro-It.ttf".into(), 0.895),
            (16, "SourceSansPro-Semibold.ttf".into(), 0.7955),
            (46, "BuilderSans-Regular.otf".into(), 0.7937),
        ];
        let mut faces = BTreeMap::new();
        for (_, file, _) in &entries {
            faces.insert(
                file.clone(),
                source_sans(file).or_else(|| Some(face(FAMILY, 400, false))),
            );
        }
        rows_to_faces(&entries, &faces)
    }

    #[test]
    fn sourcesans_3_is_upright_regular_source_sans() {
        let mut t = shipped_shape();
        add_missing_source_sans(&mut t, source_sans);
        let f = &t[&3];
        assert_eq!((f.family.as_str(), f.weight, f.italic), ("Source Sans Pro", 400, false));
    }

    #[test]
    fn sourcesans_4_is_upright_bold_source_sans() {
        let mut t = shipped_shape();
        add_missing_source_sans(&mut t, source_sans);
        let f = &t[&4];
        assert_eq!((f.family.as_str(), f.weight, f.italic), ("Source Sans Pro", 700, false));
    }

    #[test]
    fn sourcesans_5_is_upright_light_source_sans() {
        let mut t = shipped_shape();
        add_missing_source_sans(&mut t, source_sans);
        let f = &t[&5];
        assert_eq!((f.family.as_str(), f.weight, f.italic), ("Source Sans Pro", 300, false));
    }

    #[test]
    fn sourcesans_6_is_the_one_italic_and_keeps_its_slant() {
        let mut t = shipped_shape();
        add_missing_source_sans(&mut t, source_sans);
        let f = &t[&6];
        assert_eq!((f.family.as_str(), f.weight, f.italic), ("Source Sans Pro", 400, true));
        // And it is the only slanted face in the table.
        let slanted: Vec<_> = t.iter().filter(|(_, f)| f.italic).map(|(id, _)| *id).collect();
        assert_eq!(slanted, vec![6]);
    }

    #[test]
    fn the_filled_in_ids_borrow_the_source_sans_size_ratio() {
        let mut t = shipped_shape();
        add_missing_source_sans(&mut t, source_sans);
        assert_eq!(t[&3].from_rbx_font_ratio, 0.895);
        assert_eq!(t[&16].from_rbx_font_ratio, 0.7955);
    }

    #[test]
    fn an_unknown_id_draws_upright_regular_builder_sans_never_a_neighbour() {
        let mut t = shipped_shape();
        add_missing_source_sans(&mut t, source_sans);
        for id in [0, 52, 100, 999, -1] {
            assert!(!t.contains_key(&id), "id {id} must have no face of its own");
        }
        let default = pick_default(&t).expect("a Builder Sans row is present");
        assert_eq!((default.family.as_str(), default.weight, default.italic), (FAMILY, 400, false));
    }

    #[test]
    fn a_fallback_file_that_reads_as_the_wrong_weight_or_slant_is_not_used() {
        for wrong in [face("Source Sans Pro", 400, true), face("Source Sans Pro", 600, false)] {
            let mut t = shipped_shape();
            add_missing_source_sans(&mut t, |_| Some(wrong.clone()));
            for id in [3, 4, 5] {
                // 3 matches weight 400 only when upright; the italic 400 and
                // the 600 both fail every id, and the id then has no face.
                assert!(!t.contains_key(&id), "id {id} took {wrong:?}");
            }
        }
        let mut t = shipped_shape();
        add_missing_source_sans(&mut t, |_| None);
        assert!(t.get(&3).is_none() && t.get(&4).is_none() && t.get(&5).is_none());
    }

    #[test]
    fn a_row_from_the_archive_is_never_overridden_by_the_fallback() {
        let entries = vec![(3, "Other.ttf".to_owned(), 0.5)];
        let mut faces = BTreeMap::new();
        faces.insert("Other.ttf".to_owned(), Some(face("Other", 400, false)));
        let mut t = rows_to_faces(&entries, &faces);
        add_missing_source_sans(&mut t, source_sans);
        assert_eq!(t[&3].family, "Other");
    }

    #[test]
    fn a_slanted_file_is_refused_for_an_id_that_is_not_italic() {
        // A mispaired row: id 3 pointing at the italic file.
        let entries = vec![
            (3, "SourceSansPro-It.ttf".to_owned(), 0.895),
            (6, "SourceSansPro-It.ttf".to_owned(), 0.895),
        ];
        let mut faces = BTreeMap::new();
        faces.insert("SourceSansPro-It.ttf".to_owned(), source_sans("SourceSansPro-It.ttf"));
        let t = rows_to_faces(&entries, &faces);
        assert!(!t.contains_key(&3), "id 3 must not be drawn slanted");
        assert!(t[&6].italic);
    }

    /// The Login-screen capture the default rests on, kept as a regression: if
    /// slot 9 stops reading 46 there, the reasoning in the module comment has
    /// changed and the comment is now wrong.
    #[test]
    fn the_login_capture_reads_46_at_the_default_slot() {
        let username = RawTextBoxInfo {
            x: 470.0,
            y: 297.0,
            width: 340.0,
            height: 22.0,
            font_size: 16.0,
            x_alignment: 0,
            y_alignment: 1,
            text_color: 0xffd5_d5ddu32 as i32,
            font: 46,
            text_input_type: 7,
            return_key_type: 3,
            manual_focus_release: 1,
            ..Default::default()
        };
        assert_eq!(slot_value(&username, DEFAULT_FONT_SLOT), Some(46));
        assert_eq!(slot_value(&username, 6), Some(0));
    }
}
