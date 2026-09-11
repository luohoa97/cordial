// mcontext_compat.h — portable access to trap-frame registers.
//
// glibc exposes CPU registers in a signal ucontext as gregs[REG_*] (an
// indexable greg_t array). FreeBSD's mcontext_t has no such array — it uses
// named members (mc_rax, mc_rip, mc_rflags, …). This header hides that split
// behind a uniform accessor so the emulator code stays OS-agnostic.
#pragma once
#include <cstdint>
#include <ucontext.h>

namespace badcpu {

// Pointer to a general-purpose register selected by x86 encoding 0..15
// (RAX,RCX,RDX,RBX,RSP,RBP,RSI,RDI,R8..R15). Returns nullptr if out of range.
static inline uint64_t* mc_gpr(ucontext_t* ctx, unsigned reg) {
#if defined(__FreeBSD__)
    auto& m = ctx->uc_mcontext;
    switch (reg) {
        case 0:  return reinterpret_cast<uint64_t*>(&m.mc_rax);
        case 1:  return reinterpret_cast<uint64_t*>(&m.mc_rcx);
        case 2:  return reinterpret_cast<uint64_t*>(&m.mc_rdx);
        case 3:  return reinterpret_cast<uint64_t*>(&m.mc_rbx);
        case 4:  return reinterpret_cast<uint64_t*>(&m.mc_rsp);
        case 5:  return reinterpret_cast<uint64_t*>(&m.mc_rbp);
        case 6:  return reinterpret_cast<uint64_t*>(&m.mc_rsi);
        case 7:  return reinterpret_cast<uint64_t*>(&m.mc_rdi);
        case 8:  return reinterpret_cast<uint64_t*>(&m.mc_r8);
        case 9:  return reinterpret_cast<uint64_t*>(&m.mc_r9);
        case 10: return reinterpret_cast<uint64_t*>(&m.mc_r10);
        case 11: return reinterpret_cast<uint64_t*>(&m.mc_r11);
        case 12: return reinterpret_cast<uint64_t*>(&m.mc_r12);
        case 13: return reinterpret_cast<uint64_t*>(&m.mc_r13);
        case 14: return reinterpret_cast<uint64_t*>(&m.mc_r14);
        case 15: return reinterpret_cast<uint64_t*>(&m.mc_r15);
        default: return nullptr;
    }
#else
    static const int idx[16] = {
        REG_RAX, REG_RCX, REG_RDX, REG_RBX, REG_RSP, REG_RBP, REG_RSI, REG_RDI,
        REG_R8,  REG_R9,  REG_R10, REG_R11, REG_R12, REG_R13, REG_R14, REG_R15,
    };
    if (reg < 16)
        return reinterpret_cast<uint64_t*>(&ctx->uc_mcontext.gregs[idx[reg]]);
    return nullptr;
#endif
}

// Instruction pointer, as a modifiable lvalue reference.
static inline uint64_t& mc_rip(ucontext_t* ctx) {
#if defined(__FreeBSD__)
    return *reinterpret_cast<uint64_t*>(&ctx->uc_mcontext.mc_rip);
#else
    return *reinterpret_cast<uint64_t*>(&ctx->uc_mcontext.gregs[REG_RIP]);
#endif
}

// RFLAGS, as a modifiable lvalue reference.
static inline uint64_t& mc_rflags(ucontext_t* ctx) {
#if defined(__FreeBSD__)
    return *reinterpret_cast<uint64_t*>(&ctx->uc_mcontext.mc_rflags);
#else
    return *reinterpret_cast<uint64_t*>(&ctx->uc_mcontext.gregs[REG_EFL]);
#endif
}

} // namespace badcpu
