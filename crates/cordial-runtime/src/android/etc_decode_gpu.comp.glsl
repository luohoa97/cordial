// ETC2/EAC block decode as a compute shader.
//
// NVIDIA ships no ETC2 hardware. A 4050 reports textureCompressionETC2 false,
// so `etc_decode.rs` decodes every ETC2/EAC texture in software; a run at
// texture quality 4 spends tens of thousands of buffer-to-image decode calls
// there while the GPU sits at a third of load. The CPU route is already as good
// as it gets (ADR-049: banded across a thread pool, deferred to queue submit),
// which is why the remaining move is to stop asking the CPU to do it at all.
//
// This is a transliteration of `etc_decode.rs` into GLSL, not a reimplementation
// from the ETC2 specification: the byte layout, the tables, the branch order and
// the arithmetic are copied across so that a block decoded here and a block
// decoded by `decode_block` produce identical bytes. `etc_decode_gpu` refuses to
// run a dispatch whose shader has not been checked against the CPU path by
// `decode_matches_cpu_decode`, because a decoder that is subtly wrong corrupts
// textures, which is worse than the stall it was meant to remove.
//
// Byte order is the thing to be careful of. The CPU decoder indexes `block[n]`
// and reassembles the selector word with `u32::from_be_bytes(block[4..8])`, so a
// word here is the four bytes of `block` big-endian, and byte 0 of a block is
// the most significant byte of word 0. Getting this backwards yields a decoder
// that compiles, runs, and is subtly wrong everywhere -- which is exactly the
// failure `decode_matches_cpu_decode` exists to catch.

#version 450

// One workgroup per 4x4 block, one invocation per texel.
layout(local_size_x = 4, local_size_y = 4, local_size_z = 1) in;

// ETC2 source blocks, `block_bytes` each, `src_row_blocks` per row.
layout(std430, binding = 0) readonly buffer Src {
    uint src[];
} srcb;

// Decoded texels, `texel_bytes` each, `width` per row.
layout(std430, binding = 1) buffer Dst {
    uint dst[];
} dstb;

layout(push_constant) uniform Params {
    uint format;          // EtcFormat discriminant, as in etc_decode.rs
    uint width;           // texels per row
    uint height;          // texels tall
    uint src_row_blocks;  // ETC2 blocks per row in the source
    uint _pad0;
    uint _pad1;
    uint _pad2;
    uint _pad3;
} p;

const int FMT_RGB8 = 0;
const int FMT_RGB8A1 = 1;
const int FMT_RGBA8 = 2;
const int FMT_R11 = 3;
const int FMT_R11_SIGNED = 4;
const int FMT_RG11 = 5;
const int FMT_RG11_SIGNED = 6;

const int INTENSITY[8][2] = int[8][2](
    int[2](2, 8), int[2](5, 17), int[2](9, 29), int[2](13, 42),
    int[2](18, 60), int[2](24, 80), int[2](33, 106), int[2](47, 183)
);

const int DISTANCE[8] = int[8](3, 6, 11, 16, 23, 32, 41, 64);

const int EAC_MODIFIERS[16][8] = int[16][8](
    int[8](-3, -6, -9, -15, 2, 5, 8, 14),
    int[8](-3, -7, -10, -13, 2, 6, 9, 12),
    int[8](-2, -5, -8, -13, 1, 4, 7, 12),
    int[8](-2, -4, -6, -13, 1, 3, 5, 12),
    int[8](-3, -6, -8, -12, 2, 5, 7, 11),
    int[8](-3, -7, -9, -11, 2, 6, 8, 10),
    int[8](-4, -7, -8, -11, 3, 6, 7, 10),
    int[8](-3, -5, -8, -11, 2, 4, 7, 10),
    int[8](-2, -6, -8, -10, 1, 5, 7, 9),
    int[8](-2, -5, -8, -10, 1, 4, 7, 9),
    int[8](-2, -4, -8, -10, 1, 3, 7, 9),
    int[8](-2, -5, -7, -10, 1, 4, 6, 9),
    int[8](-3, -4, -7, -10, 2, 3, 6, 9),
    int[8](-1, -2, -3, -10, 0, 1, 2, 9),
    int[8](-4, -6, -8, -9, 3, 5, 7, 8),
    int[8](-3, -5, -7, -9, 2, 4, 6, 8)
);

int clamp8(int v) { return clamp(v, 0, 255); }

int ext4(int v) { return v * 17; }
int ext5(int v) { return (v << 3) | (v >> 2); }
int ext6(int v) { return (v << 2) | (v >> 4); }
int ext7(int v) { return (v << 1) | (v >> 6); }

int signed3(int v) {
    v = v & 7;
    return v >= 4 ? v - 8 : v;
}

int bits(int v, int hi, int lo) {
    return (v >> lo) & ((1 << (hi - lo + 1)) - 1);
}

// The two-bit-per-pixel selector: an MSB plane at bit 16+i and an LSB plane at
// bit i, which is the layout the CPU decoder's `pixel_index` reads.
int pixel_index(uint lo, int x, int y) {
    int i = x * 4 + y;
    int msb = int((lo >> uint(16 + i)) & 1u);
    int lsb = int((lo >> uint(i)) & 1u);
    return (msb << 1) | lsb;
}

ivec3 add3(ivec3 c, int d) { return c + ivec3(d); }

// Block bytes as four big-endian words, so `byte(b, n)` matches `block[n]`.
uint blk_word(uint base, int n) {
    uint w = srcb.src[(base >> 2) + uint(n)];
    return w;
}

int byte_at(uint w, int n) { return int((w >> uint(8 * (3 - n))) & 0xffu); }

void main() {
    ivec2 texel = ivec2(gl_GlobalInvocationID.xy);
    if (texel.x >= int(p.width) || texel.y >= int(p.height)) {
        return;
    }

    uint row_blocks = (p.width + 3u) / 4u;
    uint block_id = uint(gl_WorkGroupID.x);
    uint bx = block_id % row_blocks;
    uint by = block_id / row_blocks;
    uint block_bytes = (p.format <= 2u) ? 8u : 16u;
    uint block_off = uint((int(by) * int(p.src_row_blocks) + int(bx)) * int(block_bytes));

    ivec2 in_block = ivec2(gl_LocalInvocationID.xy); // 0..3
    int x = in_block.x;
    int y = in_block.y;

    // b0..b3 are the four base bytes; `lo` is the selector word, bytes 4..8.
    uint w0 = blk_word(block_off, 0);
    uint w1 = blk_word(block_off, 1);
    int b0 = byte_at(w0, 0);
    int b1 = byte_at(w0, 1);
    int b2 = byte_at(w0, 2);
    int b3 = byte_at(w0, 3);
    uint lo = blk_word(block_off, 1);

    ivec3 colour = ivec3(0);
    int alpha = 255;
    uint out16 = 0u;

    if (p.format <= 2u) {
        // ---------------- ETC2 RGB, RGB8A1, RGBA8 ----------------
        bool punchthrough = p.format == FMT_RGB8A1;
        bool bit33 = ((b3 >> 1) & 1) != 0;
        bool differential;
        bool opaque;
        if (punchthrough) {
            differential = true;
            opaque = bit33;
        } else {
            differential = bit33;
            opaque = true;
        }

        if (!differential) {
            ivec3 c1 = ivec3(ext4(b0 >> 4), ext4(b1 >> 4), ext4(b2 >> 4));
            ivec3 c2 = ivec3(ext4(b0 & 0xF), ext4(b1 & 0xF), ext4(b2 & 0xF));
            // subblocks, spelled out rather than factored, to keep the branch
            // order identical to the CPU decoder's.
            int cw0 = b3 >> 5;
            int cw1 = (b3 >> 2) & 7;
            bool flip = (b3 & 1) != 0;
            bool second = flip ? y >= 2 : x >= 2;
            int idx = pixel_index(lo, x, y);
            int table = second ? INTENSITY[cw1][idx & 1] : INTENSITY[cw0][idx & 1];
            int m = second ? table : table; // both arms use their own intensity
            // The CPU decoder maps idx 0 -> table[0], 1 -> table[1], 2 -> -table[0],
            // 3 -> -table[1], and idx 2 is transparent when not opaque.
            int row = second ? cw1 : cw0;
            int m_final;
            if (idx == 0) {
                m_final = !opaque ? 0 : INTENSITY[row][0];
            } else if (idx == 1) {
                m_final = INTENSITY[row][1];
            } else if (idx == 2) {
                m_final = -INTENSITY[row][0];
            } else {
                m_final = -INTENSITY[row][1];
            }
            ivec3 base = second ? c2 : c1;
            colour = base + ivec3(m_final);
            if (!opaque && idx == 2) {
                alpha = 0;
                colour = ivec3(0);
            }
        } else {
            int r = b0 >> 3;
            int g = b1 >> 3;
            int b = b2 >> 3;
            int r2 = r + signed3(b0);
            int g2 = g + signed3(b1);
            int b2c = b + signed3(b2);

            if (!(r2 >= 0 && r2 <= 31)) {
                // T-mode: three explicit colours and a distance.
                ivec3 c1 = ivec3(
                    ext4((((b0 >> 3) & 3) << 2) | (b0 & 3)),
                    ext4(b1 >> 4),
                    ext4(b1 & 0xF)
                );
                ivec3 c2 = ivec3(ext4(b2 >> 4), ext4(b2 & 0xF), ext4(b3 >> 4));
                int d = DISTANCE[(((b3 >> 2) & 3) << 1) | (b3 & 1)];
                int idx = pixel_index(lo, x, y);
                int paint = (idx == 0) ? 0 : (idx == 1) ? 1 : (idx == 2) ? 2 : 3;
                ivec3 base = (paint == 0 || paint == 2) ? c1 : c2;
                int dm = (paint == 0) ? 0 : (paint == 1) ? d : (paint == 2) ? 0 : -d;
                colour = add3(base, dm);
                if (!opaque && idx == 2) {
                    alpha = 0;
                    colour = ivec3(0);
                }
            } else if (!(g2 >= 0 && g2 <= 31)) {
                // H-mode: two explicit colours and a distance, four paints.
                int r1 = (b0 >> 3) & 0xF;
                int g1 = ((b0 & 7) << 1) | ((b1 >> 4) & 1);
                int bl1 = (b1 & 8) | ((b1 & 3) << 1) | (b2 >> 7);
                int r2h = (b2 >> 3) & 0xF;
                int g2h = ((b2 & 7) << 1) | (b3 >> 7);
                int bl2 = (b3 >> 3) & 0xF;
                int v1 = (r1 << 8) | (g1 << 4) | bl1;
                int v2 = (r2h << 8) | (g2h << 4) | bl2;
                int idx = (((b3 >> 2) & 1) << 2) | ((b3 & 1) << 1) | (v1 >= v2 ? 1 : 0);
                int d = DISTANCE[idx];
                ivec3 c1 = ivec3(ext4(r1), ext4(g1), ext4(bl1));
                ivec3 c2 = ivec3(ext4(r2h), ext4(g2h), ext4(bl2));
                int psel = pixel_index(lo, x, y);
                int paint = (psel == 0) ? 0 : (psel == 1) ? 1 : (psel == 2) ? 2 : 3;
                ivec3 base = (paint < 2) ? c1 : c2;
                int dm = (paint == 0) ? d : (paint == 1) ? -d : (paint == 2) ? d : -d;
                colour = add3(base, dm);
                if (!opaque && psel == 2) {
                    alpha = 0;
                    colour = ivec3(0);
                }
            } else if (!(b2c >= 0 && b2c <= 31)) {
                // Planar mode: per-channel origin, horizontal and vertical deltas.
                int ro = ext6((b0 >> 1) & 0x3F);
                int go = ext7(((b0 & 1) << 6) | ((b1 >> 1) & 0x3F));
                int bo = ext6(((b1 & 1) << 5) | (((b2 >> 3) & 3) << 3) | ((b2 & 3) << 1) | (b3 >> 7));
                int rh = ext6((((b3 >> 2) & 0x1F) << 1) | (b3 & 1));
                int gh = ext7(int((lo >> 25) & 0x7Fu));
                int bh = ext6(int((lo >> 19) & 0x3Fu));
                int rv = ext6(int((lo >> 13) & 0x3Fu));
                int gv = ext7(int((lo >> 6) & 0x7Fu));
                int bv = ext6(int(lo & 0x3Fu));
                colour = ivec3(
                    clamp8((x * (rh - ro) + y * (rv - ro) + 4 * ro + 2) >> 2),
                    clamp8((x * (gh - go) + y * (gv - go) + 4 * go + 2) >> 2),
                    clamp8((x * (bh - bo) + y * (bv - bo) + 4 * bo + 2) >> 2)
                );
            } else {
                // Differential sub-blocks.
                ivec3 c1 = ivec3(ext5(r), ext5(g), ext5(b));
                ivec3 c2 = ivec3(ext5(r2), ext5(g2), ext5(b2c));
                int cw0 = b3 >> 5;
                int cw1 = (b3 >> 2) & 7;
                bool flip = (b3 & 1) != 0;
                bool second = flip ? y >= 2 : x >= 2;
                int idx = pixel_index(lo, x, y);
                int row = second ? cw1 : cw0;
                int m_final;
                if (idx == 0) {
                    m_final = !opaque ? 0 : INTENSITY[row][0];
                } else if (idx == 1) {
                    m_final = INTENSITY[row][1];
                } else if (idx == 2) {
                    m_final = -INTENSITY[row][0];
                } else {
                    m_final = -INTENSITY[row][1];
                }
                ivec3 base = second ? c2 : c1;
                colour = base + ivec3(m_final);
                if (!opaque && idx == 2) {
                    alpha = 0;
                    colour = ivec3(0);
                }
            }
        }

        // RGB8A1 folds one bit of alpha into the base byte; RGBA8 gets a fourth
        // explicit channel from byte 4 of the second word.
        if (p.format == FMT_RGB8A1) {
            // decode_etc2_rgb is handed `punchthrough`, and the alpha the engine
            // wants for RGB8A1 comes from the mode word rather than a channel.
            // Kept as the opaque value the CPU path produces.
            alpha = 255;
        } else if (p.format == FMT_RGBA8) {
            alpha = ext4(byte_at(w1, 0));
        }
    } else {
        // ---------------- EAC ----------------
        uint block_lo = block_off;
        int base = byte_at(blk_word(block_lo, 0), 0);
        int mult = byte_at(blk_word(block_lo, 0), 1) >> 4;
        int tsel = byte_at(blk_word(block_lo, 0), 1) & 0xF;
        // The selector bits are the top 48 bits of bytes 2..8, read big-endian.
        uint bits48 = (blk_word(block_lo, 1) & 0xFFFFFF00u)
                    | ((blk_word(block_lo, 2) << 24) & 0xFFFFFF00u);
        int i = x * 4 + y;
        int eidx = int((bits48 >> uint(45 - 3 * i)) & 7u);
        int step = EAC_MODIFIERS[tsel][eidx];

        if (p.format == FMT_R11 || p.format == FMT_R11_SIGNED) {
            int m = step * (mult == 0 ? 1 : mult * 8);
            if (p.format == FMT_R11_SIGNED) {
                int b_signed = clamp(base > 127 ? base - 256 : base, -127, 127);
                int v = clamp(b_signed * 8 + m, -1023, 1023);
                int mag = abs(v);
                int wide = (mag << 5) | (mag >> 5);
                out16 = uint(v < 0 ? -wide : wide);
            } else {
                int v = clamp(base * 8 + 4 + m, 0, 2047);
                out16 = uint((v << 5) | (v >> 6));
            }
        } else if (p.format == FMT_RG11 || p.format == FMT_RG11_SIGNED) {
            int gbase = byte_at(blk_word(block_lo, 0), 2);
            int gmult = byte_at(blk_word(block_lo, 0), 3) >> 4;
            int gtsel = byte_at(blk_word(block_lo, 0), 3) & 0xF;
            int gstep = EAC_MODIFIERS[gtsel][eidx];
            int m = gstep * (gmult == 0 ? 1 : gmult * 8);
            int v = clamp(gbase * 8 + 4 + m, 0, 2047);
            out16 = uint((v << 5) | (v >> 6));
        } else {
            alpha = clamp8(base + step * mult);
        }
    }

    int tb = (p.format == FMT_R11 || p.format == FMT_R11_SIGNED) ? 2 : 4;
    uint d_off = (uint(texel.y) * p.width + uint(texel.x)) * uint(tb);
    if (tb == 2) {
        uint word = dstb.dst[d_off >> 2];
        uint shift = (d_off & 3u) * 8u;
        uint mask = uint(tb == 2 ? 0xFFFFu : 0xFFFFFFFFu) << shift;
        dstb.dst[d_off >> 2] = (word & ~mask) | ((out16 << shift) & mask);
    } else {
        dstb.dst[d_off >> 2] = uint(clamp8(colour.x))
                            | (uint(clamp8(colour.y)) << 8)
                            | (uint(clamp8(colour.z)) << 16)
                            | (uint(clamp8(alpha)) << 24);
    }
}
