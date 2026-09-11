#include "badcpu.h"
#include "mcontext_compat.h"
#include <cstdio>
#include <cstring>
#include <ucontext.h>
#include <unistd.h>

namespace badcpu {

static uint64_t* get_reg_ptr(ucontext_t* ctx, uint8_t reg) {
    return mc_gpr(ctx, reg);
}

static uint64_t get_reg(ucontext_t* ctx, uint8_t reg) {
    auto* p = get_reg_ptr(ctx, reg);
    return p ? *p : 0;
}

static uint64_t get_rm_value(ucontext_t* ctx, const DecodedInstruction& inst) {
    if ((inst.modrm >> 6) == 3) {
        return get_reg(ctx, inst.rm);
    }

    uint64_t addr = 0;
    if (inst.rm == 4 && inst.has_sib) {
        uint8_t scale = (inst.sib >> 6) & 0x03;
        uint8_t index = (inst.sib >> 3) & 0x07;
        uint8_t base = inst.sib & 0x07;
        // Extended registers via REX.X and REX.B
        index |= ((inst.rex >> 1) & 1) << 3;
        base |= ((inst.rex >> 0) & 1) << 3;

        uint64_t base_val = (base == 5 && (inst.modrm >> 6) == 0)
                                ? static_cast<uint64_t>(static_cast<int32_t>(inst.displacement))
                                : get_reg(ctx, base);
        uint64_t index_val = (index == 4) ? 0 : get_reg(ctx, index);
        addr = base_val + (index_val << scale);
    } else if (inst.rm == 5 && (inst.modrm >> 6) == 0) {
        addr = static_cast<uint64_t>(static_cast<int32_t>(inst.displacement));
    } else {
        addr = get_reg(ctx, inst.rm);
    }

    uint8_t mod = (inst.modrm >> 6) & 0x03;
    if (mod == 1) {
        addr += static_cast<int8_t>(inst.displacement);
    } else if (mod == 2) {
        addr += static_cast<int32_t>(inst.displacement);
    }

    return addr;
}

static EmulationResult emulate_popcnt(ucontext_t* ctx,
                                       const DecodedInstruction& inst) {
    bool is_64bit = (inst.rex & 0x08) && !inst.has_66;
    uint64_t src = get_rm_value(ctx, inst);
    uint64_t count;

    if (inst.has_66) {
        count = __builtin_popcount(static_cast<uint16_t>(src));
    } else if (is_64bit) {
        count = __builtin_popcountll(src);
    } else {
        count = __builtin_popcount(static_cast<uint32_t>(src));
    }

    *get_reg_ptr(ctx, inst.reg) = count;

    uint64_t& flags = mc_rflags(ctx);
    // POPCNT clears CF, PF, AF, ZF, SF, OF
    flags &= ~static_cast<uint64_t>(0x8D5); // bits 0(CF),2(PF),4(AF),6(ZF),7(SF),11(OF)
    if (count == 0) flags |= 0x40; // ZF

    mc_rip(ctx) += inst.len;
    return EmulationResult::Success;
}

static EmulationResult emulate_movbe(ucontext_t* ctx,
                                       const DecodedInstruction& inst) {
    bool is_64bit = (inst.rex & 0x08) && !inst.has_66;
    uint64_t addr = get_rm_value(ctx, inst);

    if ((inst.modrm >> 6) == 3) {
        uint64_t val = get_reg(ctx, inst.rm);
        if (is_64bit) {
            uint64_t swapped = __builtin_bswap64(val);
            *get_reg_ptr(ctx, inst.reg) = swapped;
        } else if (inst.has_66) {
            uint16_t swapped = __builtin_bswap16(static_cast<uint16_t>(val));
            *get_reg_ptr(ctx, inst.reg) = (*get_reg_ptr(ctx, inst.reg) & ~0xFFFFu) | swapped;
        } else {
            uint32_t swapped = __builtin_bswap32(static_cast<uint32_t>(val));
            *get_reg_ptr(ctx, inst.reg) = swapped;
        }
    } else {
        if (is_64bit) {
            uint64_t val;
            memcpy(&val, reinterpret_cast<void*>(addr), 8);
            val = __builtin_bswap64(val);
            *get_reg_ptr(ctx, inst.reg) = val;
        } else if (inst.has_66) {
            uint16_t val;
            memcpy(&val, reinterpret_cast<void*>(addr), 2);
            val = __builtin_bswap16(val);
            *get_reg_ptr(ctx, inst.reg) = (*get_reg_ptr(ctx, inst.reg) & ~0xFFFFu) | val;
        } else {
            uint32_t val;
            memcpy(&val, reinterpret_cast<void*>(addr), 4);
            val = __builtin_bswap32(val);
            *get_reg_ptr(ctx, inst.reg) = val;
        }
    }

    mc_rip(ctx) += inst.len;
    return EmulationResult::Success;
}

static EmulationResult emulate_lzcnt_tzcnt(ucontext_t* ctx,
                                              const DecodedInstruction& inst, bool is_lzcnt) {
    bool is_64bit = (inst.rex & 0x08) && !inst.has_66;
    uint64_t src = get_rm_value(ctx, inst);
    uint64_t result;

    if (is_lzcnt) {
        if (src == 0) {
            result = is_64bit ? 64 : (inst.has_66 ? 16 : 32);
        } else if (is_64bit) {
            result = __builtin_clzll(src);
        } else if (inst.has_66) {
            result = __builtin_clz(static_cast<uint16_t>(src)) - 16;
        } else {
            result = __builtin_clz(static_cast<uint32_t>(src));
        }
    } else {
        if (src == 0) {
            result = is_64bit ? 64 : (inst.has_66 ? 16 : 32);
        } else if (is_64bit) {
            result = __builtin_ctzll(src);
        } else if (inst.has_66) {
            result = __builtin_ctz(static_cast<uint16_t>(src));
        } else {
            result = __builtin_ctz(static_cast<uint32_t>(src));
        }
    }

    uint64_t& flags = mc_rflags(ctx);
    // LZCNT/TZCNT clear OF, SF, AF, PF, CF; ZF set if src==0
    flags &= ~static_cast<uint64_t>(0x8D1); // bits 0(CF),4(AF),6(ZF),7(SF),11(OF) ... wait PF is bit 2
    // Actually: CF=0, PF=0, AF=0, SF=0, OF=0, ZF=1 if src==0
    flags &= ~static_cast<uint64_t>(0x8D5); // CF(0), PF(2), AF(4), ZF(6), SF(7), OF(11)
    if (src == 0) flags |= 0x40; // ZF

    *get_reg_ptr(ctx, inst.reg) = result;
    mc_rip(ctx) += inst.len;
    return EmulationResult::Success;
}

static EmulationResult emulate_bmi(uint8_t op3, ucontext_t* ctx,
                                     const DecodedInstruction& inst) {
    bool is_64bit = (inst.rex & 0x08) && !inst.has_66;
    uint64_t src1 = get_reg(ctx, inst.reg);
    uint64_t src2 = get_rm_value(ctx, inst);
    uint64_t result;

    // BMI1 uses VEX encoding: src1 is reg (VEX.vvvv), src2 is r/m
    switch (op3) {
        case 0xF2: // ANDN
            result = src1 & ~src2;
            break;
        case 0xF3: // BLSI
            result = src2 & (-src2);
            break;
        case 0xF1: // BLSMSK
            result = src2 ^ (src2 - 1);
            break;
        case 0xF4: // BLSR
            result = src2 & (src2 - 1);
            break;
        default:
            return EmulationResult::UnrecognizedInstruction;
    }

    if (!is_64bit) {
        result &= 0xFFFFFFFFu;
    }

    *get_reg_ptr(ctx, inst.reg) = result;

    uint64_t& flags = mc_rflags(ctx);
    // BMI1 clears OF, SF, AF, PF, CF; ZF set if result==0, SF set from MSB of result
    flags &= ~static_cast<uint64_t>(0x8D5); // clear CF, PF, AF, ZF, SF, OF
    if (result == 0) flags |= 0x40; // ZF
    if (result & (is_64bit ? (1ull << 63) : (1u << 31))) flags |= 0x80; // SF

    mc_rip(ctx) += inst.len;
    return EmulationResult::Success;
}

EmulationResult emulate_instruction(const uint8_t* ip, const CpuFeatures& features,
                                      ucontext_t* ctx) {
    DecodedInstruction inst = decode_instruction(ip);

    if (inst.is_vex) {
        uint8_t op3 = inst.opcode[2];
        if (inst.opcode[0] == 0x0F && inst.opcode[1] == 0x38) {
            // VEX-encoded BMI1/2 instructions in 0F38 map
            if (op3 == 0xF2 || op3 == 0xF3 || op3 == 0xF1 || op3 == 0xF4) {
                return emulate_bmi(op3, ctx, inst);
            }
        }
    }

    if (inst.opcode[0] == 0x0F) {
        uint8_t op2 = inst.opcode[1];

        // POPCNT r, r/m (F3 0F B8 /r)
        if (op2 == 0xB8 && inst.has_modrm && inst.has_f3) {
            if (!features.has_popcnt()) {
                return emulate_popcnt(ctx, inst);
            }
        }

        // MOVBE (0F 38 F0/F1)
        if (op2 == 0x38) {
            uint8_t op3 = inst.opcode[2];
            if (op3 == 0xF0 || op3 == 0xF1) {
                return emulate_movbe(ctx, inst);
            }
            // LZCNT (0F 38 F5 with F3 prefix)
            if (op3 == 0xF5 && inst.has_f3) {
                return emulate_lzcnt_tzcnt(ctx, inst, true);
            }
        }

        // TZCNT (0F BC with F3 prefix)
        if (op2 == 0xBC && inst.has_f3) {
            return emulate_lzcnt_tzcnt(ctx, inst, false);
        }
        // LZCNT (0F BD with F3 prefix)
        if (op2 == 0xBD && inst.has_f3) {
            return emulate_lzcnt_tzcnt(ctx, inst, true);
        }
    }

    return EmulationResult::UnrecognizedInstruction;
}

void fatal_unrecognized_instruction(const uint8_t* ip, size_t max_len) {
    fprintf(stderr, "---------------------------------------------------------\n");
    fprintf(stderr, "FATAL: Unrecognized Instruction\n");
    fprintf(stderr, "Instruction Bytes:");
    for (size_t i = 0; i < max_len && i < 16; i++) {
        fprintf(stderr, " %02x", ip[i]);
    }
    fprintf(stderr, "\n");
    fprintf(stderr, 
        "While your CPU supports SSE 4.1, illegal instruction errors can also "
        "be thrown due to the program encountering something it didn't expect "
        "as a way to quickly stop itself. Check your configuration, verify file "
        "integrity, and try again. If nothing works, you should probably report "
        "this as a bug.\n");
    _exit(1);
}

} // namespace badcpu
