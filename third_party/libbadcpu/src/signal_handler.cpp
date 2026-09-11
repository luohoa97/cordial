#include "badcpu.h"
#include "mcontext_compat.h"
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <unistd.h>

namespace badcpu {

static CpuFeatures g_cpu_features{};
static bool g_handler_installed = false;
static struct sigaction g_old_sigill{};

static void sigill_handler(int sig, siginfo_t* info, void* ucontext) {
    if (sig != SIGILL) {
        return;
    }

    auto* ctx = static_cast<ucontext_t*>(ucontext);
    auto* ip = reinterpret_cast<uint8_t*>(mc_rip(ctx));

    EmulationResult result = emulate_instruction(ip, g_cpu_features, ctx);

    switch (result) {
        case EmulationResult::Success:
            return;

        case EmulationResult::UnrecognizedInstruction:
            fprintf(stderr, "---------------------------------------------------------\n");
            fprintf(stderr, "FATAL: Unrecognized Instruction\n");
            fprintf(stderr, "Instruction Bytes:");
            for (int i = 0; i < 16; i++) {
                fprintf(stderr, " %02x", ip[i]);
            }
            fprintf(stderr, "\n");
            fprintf(stderr,
                "We attempted to emulate missing features, but encountered "
                "an instruction that could not be handled.\n");
            fprintf(stderr, "Received signal: %d (Illegal Instruction)\n", sig);
            _exit(128 + SIGILL);

        case EmulationResult::UnsupportedCpu:
            fprintf(stderr, "This program requires a CPU with x86-64-v2 features.\n");
            fprintf(stderr, "Your CPU does not support at least SSE 4.1, which is "
                            "required for this software to work.\n");
            _exit(1);

        default:
            fprintf(stderr, "Error in SIGILL handler\n");
            _exit(1);
    }
}

bool install_sigill_handler() {
    if (g_handler_installed) {
        return true;
    }

    g_cpu_features = detect_cpu_features();

    if (!g_cpu_features.has_sse41()) {
        fprintf(stderr, "This program requires a CPU with x86-64-v2 features.\n");
        fprintf(stderr, "Your CPU does not support at least SSE 4.1, which is "
                        "required for this software to work.\n");
        return false;
    }

    print_cpu_info(g_cpu_features);

    struct sigaction sa{};
    sa.sa_sigaction = sigill_handler;
    sa.sa_flags = SA_SIGINFO;
    sigemptyset(&sa.sa_mask);

    if (sigaction(SIGILL, &sa, &g_old_sigill) != 0) {
        perror("Error setting SIGILL handler");
        return false;
    }

    g_handler_installed = true;
    return true;
}

void remove_sigill_handler() {
    if (g_handler_installed) {
        sigaction(SIGILL, &g_old_sigill, nullptr);
        g_handler_installed = false;
    }
}

} // namespace badcpu

__attribute__((constructor)) static void libbadcpu_init() {
    badcpu::install_sigill_handler();
}

__attribute__((destructor)) static void libbadcpu_fini() {
    badcpu::remove_sigill_handler();
}