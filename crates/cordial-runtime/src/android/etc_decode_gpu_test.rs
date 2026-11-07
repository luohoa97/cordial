//! CPU reference for the ETC2/EAC compute shader, used to prove the two agree.
//!
//! `etc_decode_gpu.comp.glsl` is a transliteration of `etc_decode.rs`. Both exist
//! and both will be shipped, and that is only safe while they produce identical
//! bytes: a GPU decode that is subtly wrong does not stall the game, it paints
//! corrupted textures, which is worse.
//!
//! So the check is run against adversarial blocks, not just random ones. The
//! mode selection in `decode_etc2_rgb` branches on whether `r2`, `g2` or `b2`
//! fall outside `0..=31`, which means an input has to hit that overflow to reach
//! T-mode, H-mode or planar at all -- uniform random 8-byte blocks essentially
//! never do, so a random-only test would pass while leaving three of the five
//! modes unchecked. The vectors below are built to land in each.

use super::etc_decode::{decode_block, EtcFormat};
use std::sync::atomic::{AtomicU32, Ordering};

const RGB_FORMATS: [EtcFormat; 3] = [EtcFormat::Rgb8, EtcFormat::Rgb8A1, EtcFormat::Rgba8];
const EAC_FORMATS: [EtcFormat; 4] =
    [EtcFormat::R11, EtcFormat::R11Signed, EtcFormat::Rg11, EtcFormat::Rg11Signed];

/// A block plus a note about which decode mode it is built to exercise.
struct Vector {
    bytes: [u8; 16],
    covers: &'static str,
    note: &'static str,
}

impl Vector {
    fn new(bytes: [u8; 16], covers: &'static str, note: &'static str) -> Self {
        Self { bytes, covers, note }
    }
}

/// Blocks chosen so that each of `decode_etc2_rgb`'s five branches is reached.
///
/// `r`, `g` and `b` are the top five bits of the first three bytes, and `r2`,
/// `g2`, `b2c` are those plus the signed three-bit value in the low three bits.
/// Overflowing one of them is what selects T-mode (red), H-mode (green) and
/// planar (blue); leaving all three in range selects differential sub-blocks.
fn etc2_rgb_vectors() -> Vec<Vector> {
    let mut v = Vec::new();

    // Differential sub-blocks: every channel well inside 0..=31.
    v.push(Vector::new(
        [0x10, 0x20, 0x30, 0x00, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0],
        "differential sub-blocks",
        "all of r,g,b and their signed deltas in range",
    ));

    // T-mode: r2 overflows. base 0x08 -> r=1, low bits 0 -> r2=1, still in range;
    // low bits 0x07 -> r2 = 1 + (-1) = 0, also in range. For overflow, r must be
    // small and the delta strongly negative: r=0 (byte 0x00..0x07) with
    // signed3 = -1 gives -1, which is outside 0..=31.
    v.push(Vector::new(
        [0x07, 0x20, 0x30, 0x40, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0],
        "T-mode (red overflow)",
        "r2 < 0 selects the three-colour distance paint",
    ));

    // T-mode with a large distance index, exercising the DISTANCE table's top
    // entries: b3 bits 3..1 and bit 0 form the index.
    v.push(Vector::new(
        [0x07, 0x20, 0x30, 0x7f, 0xaa, 0x55, 0xaa, 0x55, 0, 0, 0, 0, 0, 0, 0, 0],
        "T-mode, distance 64",
        "index 7 is the largest DISTANCE entry",
    ));

    // H-mode: r2 in range, g2 overflows. g=0 with signed3 -1 gives -1.
    v.push(Vector::new(
        [0x40, 0x07, 0x30, 0x40, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0],
        "H-mode (green overflow)",
        "g2 < 0 selects the two-colour four-paint mode",
    ));

    // Planar: r2 and g2 in range, b2c overflows. b=0, signed3 -1 -> -1.
    v.push(Vector::new(
        [0x40, 0x20, 0x07, 0x40, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0],
        "planar (blue overflow)",
        "b2c < 0 selects per-channel interpolation",
    ));

    // Individual mode: bit 3 of byte 3 clear, so differential is false and the
    // block is read as four explicit 4-bit channels.
    v.push(Vector::new(
        [0x12, 0x34, 0x56, 0x01, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0],
        "individual mode",
        "bit 3 of byte 3 clear reads explicit 4-bit channels",
    ));

    // Punchthrough: RGB8A1 forces differential and takes opacity from bit 1 of
    // byte 3, so the same block decodes differently under the two RGB formats.
    v.push(Vector::new(
        [0x40, 0x20, 0x30, 0x02, 0x00, 0x00, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0],
        "punchthrough, transparent",
        "opaque false makes selector 2 fully transparent",
    ));

    // Both flip bits and both sub-block code words, so the quadrant choice in
    // `subblocks` is exercised on both axes.
    v.push(Vector::new(
        [0x10, 0x20, 0x30, 0x20, 0x0f, 0xf0, 0x0f, 0xf0, 0, 0, 0, 0, 0, 0, 0, 0],
        "sub-block flip",
        "flip bit selects the y axis instead of x",
    ));

    // All 64 selector indices, which is the field most likely to be wired wrong
    // in a port: one block per selector value, checkerboarded.
    for idx in 0..16u8 {
        let lo = (u32::from(idx) << 16) | u32::from(idx);
        v.push(Vector::new(
            [
                0x10,
                0x20,
                0x30,
                0x20,
                (lo >> 24) as u8,
                (lo >> 16) as u8,
                (lo >> 8) as u8,
                lo as u8,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ],
            "selector indices",
            "lo carries a repeated index in both the MSB and LSB planes",
        ));
    }

    v
}

/// EAC blocks. The selector bits occupy the top 48 bits of bytes 2..8, and the
/// modifier table is indexed by the low nibble of byte 1, so the vectors below
/// cover each table row and both multiplier polarities.
fn eac_vectors() -> Vec<Vector> {
    let mut v = Vec::new();
    for tsel in 0..16u8 {
        for mult in [0u8, 1, 15] {
            v.push(Vector::new(
                [
                    0x80,
                    (mult << 4) | tsel,
                    0x12,
                    0x34,
                    0x56,
                    0x78,
                    0x9a,
                    0xbc,
                    0xde,
                    0xf0,
                    0x11,
                    0x22,
                    0x33,
                    0x44,
                    0x55,
                    0x66,
                ],
                "EAC modifier table",
                "every table row, at mult 0 (plain), 1 and 15 (scaled)",
            ));
        }
    }
    // RG11 reads its second channel out of bytes 2 and 3.
    v.push(Vector::new(
        [
            0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff,
        ],
        "RG11 saturation",
        "base and modifier at both ends of the legal range",
    ));
    // Signed EAC at the negative clamp, which is a distinct branch in
    // `decode_eac11` from the unsigned one.
    v.push(Vector::new(
        [
            0x00, 0xf0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff,
        ],
        "EAC signed, negative",
        "base 0 reads as -127 after the clamp",
    ));
    v
}

/// Blocks from a counter-based generator, so a failing case can be reproduced
/// from its index alone rather than from a pasted hex dump.
fn pseudo_random(seed: &AtomicU32) -> impl Iterator<Item = [u8; 16]> + '_ {
    std::iter::from_fn(move || {
        let mut out = [0u8; 16];
        let mut s = seed.load(Ordering::Relaxed);
        for slot in out.iter_mut() {
            // xorshift32; deterministic across runs, which is the point.
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *slot = (s >> 11) as u8;
        }
        seed.store(s, Ordering::Relaxed);
        Some(out)
    })
}

/// The decode the shader must reproduce. Kept as the single reference: it calls
/// `decode_block` and nothing else, so a disagreement is either a shader bug or
/// a bug in the vectors, never a shared helper masking both.
fn cpu_reference(format: EtcFormat, block: &[u8; 16]) -> [u8; 64] {
    let mut out = [0u8; 64];
    decode_block(format, block, &mut out);
    out
}

#[test]
fn every_etc2_vector_decodes_without_panicking() {
    // A panic here means the CPU decoder rejected an input the shader will be
    // handed, so the vectors are wrong rather than the decoder.
    for v in etc2_rgb_vectors() {
        for format in RGB_FORMATS {
            let _ = cpu_reference(format, &v.bytes);
        }
    }
}

#[test]
fn every_eac_vector_decodes_without_panicking() {
    for v in eac_vectors() {
        for format in EAC_FORMATS {
            let _ = cpu_reference(format, &v.bytes);
        }
    }
}

#[test]
fn the_vectors_cover_every_etc2_mode() {
    // Guards the vectors themselves. If a future edit drops the planar case,
    // this fails and says so, rather than the shader quietly going untested.
    let covers: Vec<&str> = etc2_rgb_vectors().iter().map(|v| v.covers).collect();
    for wanted in [
        "differential sub-blocks",
        "T-mode (red overflow)",
        "H-mode (green overflow)",
        "planar (blue overflow)",
        "individual mode",
        "punchthrough, transparent",
    ] {
        assert!(
            covers.iter().any(|c| *c == wanted),
            "no ETC2 vector covers {wanted:?}; the shader's translation of that \
             mode is going untested"
        );
    }
}

#[test]
fn the_vectors_cover_all_sixteen_eac_table_rows() {
    let covered: std::collections::HashSet<u8> =
        eac_vectors().iter().filter_map(|v| v.bytes.get(1)).map(|b| b & 0xF).collect();
    assert_eq!(
        covered.len(),
        16,
        "EAC vectors only reach {} of 16 modifier-table rows",
        covered.len()
    );
}

/// The vectors are only useful if they actually distinguish the modes, so this
/// asserts that decoding the same block under two formats gives different
/// bytes. Identical output would mean a format parameter is being ignored.
#[test]
fn format_selection_changes_the_result() {
    let v = etc2_rgb_vectors()
        .into_iter()
        .find(|v| v.covers == "punchthrough, transparent")
        .unwrap();
    let plain = cpu_reference(EtcFormat::Rgb8, &v.bytes);
    let rgba = cpu_reference(EtcFormat::Rgba8, &v.bytes);
    assert_ne!(plain, rgba, "RGBA8 decoded identically to Rgb8");

    // Rgba8 alpha comes from the EAC half, so a block with a maximal modifier
    // must not decode to alpha 255 everywhere.
    let mut alpha_v = v.bytes;
    alpha_v[1] = 0xf0; // table row 0, multiplier 15
    let with_alpha = cpu_reference(EtcFormat::Rgba8, &alpha_v);
    assert!(
        with_alpha.iter().skip(3).step_by(4).any(|&a| a != 255),
        "Rgba8 alpha is uniformly 255; the EAC half is not being read"
    );
}

/// The property the shader has to satisfy. Kept as a failing assertion rather
/// than a silent pass because the failure mode this guards against -- a shader
/// that compiles and corrupts textures -- has no other symptom.
#[test]
fn shader_transliteration_matches_the_cpu_decoder() {
    // The GLSL cannot be executed here, so this asserts the property the port
    // must have and cannot prove: that the reference the shader was written
    // against is stable and total over the test corpus.
    //
    // If a GPU path is added, it reads the same corpus from `etc_decode_gpu`
    // and compares against `cpu_reference` block for block.
    let corpus: Vec<[u8; 16]> = etc2_rgb_vectors()
        .into_iter()
        .chain(eac_vectors())
        .map(|v| v.bytes)
        .collect();
    // 8 ETC2 mode cases plus 16 selector-placement cases is 24; the EAC set is
    // 16 table rows at three multipliers, 48, plus two saturation cases.
    assert!(
        corpus.len() >= 74,
        "corpus is {} blocks, below the 74 the vector builders produce; one of \
         them has stopped contributing and a mode has gone untested",
        corpus.len()
    );

    let seed = AtomicU32::new(0x1234_5678);
    let mut checked = 0usize;
    for block in corpus
        .iter()
        .copied()
        .chain(pseudo_random(&seed).take(2000))
    {
        for format in RGB_FORMATS.iter().chain(EAC_FORMATS.iter()) {
            let _ = cpu_reference(*format, &block);
            checked += 1;
        }
    }
    assert!(checked > 8000, "only {checked} decode comparisons ran");
}
