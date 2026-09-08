// PipeWire, reached without ever appearing in `libcordial_liblog.a`'s
// `DT_NEEDED`.
//
// Two separate "optional" decisions are stacked here and it is worth being
// precise about which is which:
//
// 1. **Compile time.** `CMakeLists.txt` looks for `libpipewire-0.3` and
//    `spa-0.2` with `pkg_check_modules` and defines `CORDIAL_HAVE_PIPEWIRE`
//    only if it finds them. If it does not — a machine with no
//    `pipewire-devel` installed — this whole file compiles down to the
//    fallback at the bottom, and the rest of the tree builds exactly as it
//    did before this file existed. Everything above that `#else` uses the
//    real headers, because writing out `spa_pod`'s binary layout by hand
//    when the header that does it correctly is one `pkg-config` call away
//    is not a trade worth making.
//
// 2. **Link and run time.** Even when the headers were present at compile
//    time, `libpipewire-0.3.so` is never linked. Every function in it that
//    this file calls is `dlsym`'d through `load_library()` below, and a
//    missing library or a missing symbol degrades to "no audio" rather than
//    a failure to start. This is what makes a Cordial binary built on a
//    PipeWire machine still run — audio-less — on one that has neither the
//    daemon nor the library.
//
// A third category of PipeWire call needs neither: `pw_core_add_listener`,
// `pw_core_sync` and friends are `static inline` in `<pipewire/core.h>`,
// reading a method table embedded in the `pw_core` object itself (the same
// `spa_interface` pattern used throughout SPA). Those compile straight into
// this translation unit and never touch `dlsym` at all — only the functions
// that *construct* those objects (`pw_context_connect`, `pw_stream_new_simple`,
// ...) are real, exported, and therefore looked up by name.

#include "pipewire_backend.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>

#ifdef CORDIAL_HAVE_PIPEWIRE

#include <spa/param/audio/format-utils.h>
#include <spa/param/props.h>
#include <spa/utils/result.h>
#include <pipewire/pipewire.h>
#include <pipewire/extensions/metadata.h>

#include <dlfcn.h>

#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <mutex>
#include <string>
#include <vector>

namespace cordial::audio {
namespace {

// ----------------------------------------------------------------- library

/// Every libpipewire-0.3 entry point this file calls, resolved by name at
/// run time. Grouped as plain fields rather than looked up per call: a
/// missing symbol is discovered once, at `load_library()`, rather than as a
/// null-pointer call partway through a stream's lifetime.
struct Library {
    void* handle = nullptr;

    void (*init)(int*, char***);
    void (*deinit)();

    pw_thread_loop* (*thread_loop_new)(const char*, const spa_dict*);
    void (*thread_loop_destroy)(pw_thread_loop*);
    int (*thread_loop_start)(pw_thread_loop*);
    void (*thread_loop_stop)(pw_thread_loop*);
    void (*thread_loop_lock)(pw_thread_loop*);
    void (*thread_loop_unlock)(pw_thread_loop*);
    int (*thread_loop_timed_wait)(pw_thread_loop*, int);
    void (*thread_loop_signal)(pw_thread_loop*, bool);
    pw_loop* (*thread_loop_get_loop)(pw_thread_loop*);

    pw_context* (*context_new)(pw_loop*, pw_properties*, size_t);
    void (*context_destroy)(pw_context*);
    pw_core* (*context_connect)(pw_context*, pw_properties*, size_t);
    int (*core_disconnect)(pw_core*);

    pw_properties* (*properties_new)(const char*, ...);

    pw_stream* (*stream_new_simple)(pw_loop*, const char*, pw_properties*,
                                     const pw_stream_events*, void*);
    void (*stream_destroy)(pw_stream*);
    int (*stream_connect)(pw_stream*, spa_direction, uint32_t, pw_stream_flags,
                           const spa_pod**, uint32_t);
    int (*stream_disconnect)(pw_stream*);
    pw_buffer* (*stream_dequeue_buffer)(pw_stream*);
    int (*stream_queue_buffer)(pw_stream*, pw_buffer*);
    int (*stream_set_active)(pw_stream*, bool);
    int (*stream_update_params)(pw_stream*, const spa_pod**, uint32_t);

    // Device enumeration only. The registry and metadata objects it walks are
    // reached through `pw_core_get_registry` / `pw_registry_bind`, which are
    // static inline (see the file header on the third category), but the
    // proxies they hand back are real objects that need a real, exported
    // destructor — and enumeration runs every time Roblox asks for the device
    // list, so leaking one per call would be a leak per device-picker open.
    void (*proxy_destroy)(pw_proxy*);
};

Library g_lib{};

bool load_library() {
    // The SONAME every distribution that ships PipeWire installs; the bare
    // name is a fallback for the rare tree that only has the dev symlink.
    void* handle = dlopen("libpipewire-0.3.so.0", RTLD_NOW | RTLD_GLOBAL);
    if (!handle) handle = dlopen("libpipewire-0.3.so", RTLD_NOW | RTLD_GLOBAL);
    if (!handle) {
        std::fprintf(stderr,
            "E/Cordial-Audio           libpipewire-0.3 not found (%s); Roblox's audio "
            "has nowhere to go. Install PipeWire and its client library to enable it.\n",
            dlerror());
        return false;
    }

    struct Entry { const char* name; void** slot; };
    const Entry entries[] = {
        {"pw_init", reinterpret_cast<void**>(&g_lib.init)},
        {"pw_deinit", reinterpret_cast<void**>(&g_lib.deinit)},
        {"pw_thread_loop_new", reinterpret_cast<void**>(&g_lib.thread_loop_new)},
        {"pw_thread_loop_destroy", reinterpret_cast<void**>(&g_lib.thread_loop_destroy)},
        {"pw_thread_loop_start", reinterpret_cast<void**>(&g_lib.thread_loop_start)},
        {"pw_thread_loop_stop", reinterpret_cast<void**>(&g_lib.thread_loop_stop)},
        {"pw_thread_loop_lock", reinterpret_cast<void**>(&g_lib.thread_loop_lock)},
        {"pw_thread_loop_unlock", reinterpret_cast<void**>(&g_lib.thread_loop_unlock)},
        {"pw_thread_loop_timed_wait", reinterpret_cast<void**>(&g_lib.thread_loop_timed_wait)},
        {"pw_thread_loop_signal", reinterpret_cast<void**>(&g_lib.thread_loop_signal)},
        {"pw_thread_loop_get_loop", reinterpret_cast<void**>(&g_lib.thread_loop_get_loop)},
        {"pw_context_new", reinterpret_cast<void**>(&g_lib.context_new)},
        {"pw_context_destroy", reinterpret_cast<void**>(&g_lib.context_destroy)},
        {"pw_context_connect", reinterpret_cast<void**>(&g_lib.context_connect)},
        {"pw_core_disconnect", reinterpret_cast<void**>(&g_lib.core_disconnect)},
        {"pw_properties_new", reinterpret_cast<void**>(&g_lib.properties_new)},
        {"pw_stream_new_simple", reinterpret_cast<void**>(&g_lib.stream_new_simple)},
        {"pw_stream_destroy", reinterpret_cast<void**>(&g_lib.stream_destroy)},
        {"pw_stream_connect", reinterpret_cast<void**>(&g_lib.stream_connect)},
        {"pw_stream_disconnect", reinterpret_cast<void**>(&g_lib.stream_disconnect)},
        {"pw_stream_dequeue_buffer", reinterpret_cast<void**>(&g_lib.stream_dequeue_buffer)},
        {"pw_stream_queue_buffer", reinterpret_cast<void**>(&g_lib.stream_queue_buffer)},
        {"pw_stream_set_active", reinterpret_cast<void**>(&g_lib.stream_set_active)},
        {"pw_stream_update_params", reinterpret_cast<void**>(&g_lib.stream_update_params)},
        {"pw_proxy_destroy", reinterpret_cast<void**>(&g_lib.proxy_destroy)},
    };
    for (const Entry& e : entries) {
        *e.slot = dlsym(handle, e.name);
        if (!*e.slot) {
            std::fprintf(stderr,
                "E/Cordial-Audio           libpipewire-0.3 is missing '%s'; treating the "
                "whole library as unusable rather than calling through a null pointer. "
                "No audio output.\n", e.name);
            dlclose(handle);
            return false;
        }
    }
    g_lib.handle = handle;
    return true;
}

// ------------------------------------------------------------------ session

/// One PipeWire connection for the whole process, matching how Roblox itself
/// only ever creates one OpenSL engine. Never torn down: it lives exactly as
/// long as `cordial_liblog`'s other process-lifetime state (bionic's TLS,
/// the linker's loaded-library table) does.
///
/// The core listener and its round-trip state are *members* rather than the
/// locals they used to be, and that is a correctness fix, not tidying.
/// `pw_core_add_listener` links the `spa_hook` it is given into a list the
/// core keeps and dereferences on every later core event; the previous
/// revision passed a hook that lived on `connect_session`'s stack and never
/// removed it, so the list held a pointer into a frame that had returned.
/// Nothing had noticed because nothing ever issued a second `pw_core_sync` —
/// device enumeration is the first thing to do so, and it turned a latent
/// dangling pointer into one that would actually be called.
struct Session {
    pw_thread_loop* loop = nullptr;
    pw_context* context = nullptr;
    pw_core* core = nullptr;

    spa_hook core_listener{};
    int pending_seq = -1;
    bool sync_done = false;
    bool sync_failed = false;
};

/// Blocks until the server has answered a `pw_core_sync`, i.e. until every
/// request issued before it has been processed and every event it produced
/// has been delivered. Must be called with `loop` locked.
///
/// Three seconds, matching `connect_session`: a session that cannot answer a
/// local socket round trip in that time is not one worth making Roblox wait
/// on. False means the answer never came, and every caller treats that as
/// "report what was gathered so far" rather than retrying — a wedged daemon
/// does not become unwedged by being asked twice.
bool round_trip(Session* s) {
    s->sync_done = false;
    s->sync_failed = false;
    s->pending_seq = pw_core_sync(s->core, PW_ID_CORE, 0);
    while (!s->sync_done && !s->sync_failed) {
        if (g_lib.thread_loop_timed_wait(s->loop, 3) != 0) break;
    }
    return s->sync_done;
}

/// Connects and round-trips once. A `pw_context_connect` that returns a
/// non-null `pw_core` only proves a socket was opened — a stale
/// `PIPEWIRE_RUNTIME_DIR` pointing at a dead socket directory, or a daemon
/// that accepted the connection but is wedged, both get this far. The
/// `pw_core_sync`/`done` round trip is what actually proves someone is home;
/// see the "Degrade honestly" requirement this exists to satisfy.
Session* connect_session() {
    if (!load_library()) return nullptr;

    g_lib.init(nullptr, nullptr);

    pw_thread_loop* loop = g_lib.thread_loop_new("cordial-pipewire", nullptr);
    if (!loop) {
        std::fprintf(stderr,
            "E/Cordial-Audio           pw_thread_loop_new failed; no audio output.\n");
        return nullptr;
    }
    if (g_lib.thread_loop_start(loop) < 0) {
        std::fprintf(stderr,
            "E/Cordial-Audio           pw_thread_loop_start failed; no audio output.\n");
        g_lib.thread_loop_destroy(loop);
        return nullptr;
    }

    g_lib.thread_loop_lock(loop);

    pw_context* context = g_lib.context_new(g_lib.thread_loop_get_loop(loop), nullptr, 0);
    if (!context) {
        std::fprintf(stderr,
            "E/Cordial-Audio           pw_context_new failed; no audio output.\n");
        g_lib.thread_loop_unlock(loop);
        g_lib.thread_loop_stop(loop);
        g_lib.thread_loop_destroy(loop);
        return nullptr;
    }

    pw_core* core = g_lib.context_connect(context, nullptr, 0);
    if (!core) {
        std::fprintf(stderr,
            "E/Cordial-Audio           no PipeWire session reachable (is a PipeWire "
            "daemon running, and is PIPEWIRE_RUNTIME_DIR/XDG_RUNTIME_DIR set?); "
            "no audio output.\n");
        g_lib.thread_loop_unlock(loop);
        g_lib.context_destroy(context);
        g_lib.thread_loop_stop(loop);
        g_lib.thread_loop_destroy(loop);
        return nullptr;
    }

    // Built before the listener is registered, and never freed, because the
    // core keeps a pointer to `session->core_listener` for as long as the
    // connection lives. See the Session comment on why that used to be a
    // stack local and why it must not be.
    auto* session = new Session();
    session->loop = loop;
    session->context = context;
    session->core = core;

    static pw_core_events core_events = [] {
        pw_core_events e{};
        e.version = PW_VERSION_CORE_EVENTS;
        e.done = [](void* data, uint32_t id, int seq) {
            auto* s = static_cast<Session*>(data);
            if (id == PW_ID_CORE && seq == s->pending_seq) {
                s->sync_done = true;
                g_lib.thread_loop_signal(s->loop, false);
            }
        };
        e.error = [](void* data, uint32_t id, int, int res, const char* message) {
            auto* s = static_cast<Session*>(data);
            std::fprintf(stderr,
                "E/Cordial-Audio           PipeWire core reported an error "
                "(id=%u res=%d %s).\n", id, res, message ? message : "");
            s->sync_failed = true;
            g_lib.thread_loop_signal(s->loop, false);
        };
        return e;
    }();
    pw_core_add_listener(core, &session->core_listener, &core_events, session);

    bool reachable = round_trip(session);

    g_lib.thread_loop_unlock(loop);

    if (!reachable) {
        if (!session->sync_failed) {
            std::fprintf(stderr,
                "E/Cordial-Audio           PipeWire did not answer within 3s; treating "
                "the session as unreachable. No audio output.\n");
        }
        g_lib.thread_loop_lock(loop);
        spa_hook_remove(&session->core_listener);
        g_lib.core_disconnect(core);
        g_lib.context_destroy(context);
        g_lib.thread_loop_unlock(loop);
        g_lib.thread_loop_stop(loop);
        g_lib.thread_loop_destroy(loop);
        delete session;
        return nullptr;
    }

    // **What this line may say is what just happened, and no more.** It used to
    // finish "OpenSL ES audio players will play through it", tagged
    // `Cordial-OpenSLES`, and both halves were wrong on most runs: this session
    // is the one shared session, `pipewire_available()` is what
    // `supportsAAudio()` and the device enumeration in `audio_classes.cpp` both
    // ask, and either of those brings it up. So a run that had selected AAudio
    // and would never call `slCreateEngine` printed a promise about OpenSL ES
    // two lines under "audio backend: aaudio", which is exactly the kind of
    // reading that has to be believed before it can be checked.
    //
    // Nothing is lost by dropping the promise: which backend is in use is
    // announced by `cordial_audio_backend_announce` on its own line, from the
    // place that actually decides it.
    std::fprintf(stderr,
        "I/Cordial-Audio           PipeWire session reachable: the client library loaded "
        "and a core round trip completed. This one session is shared by every audio path "
        "that reaches PipeWire.\n");
    return session;
}

Session* get_session() {
    static Session* session = connect_session();
    return session;
}

// -------------------------------------------------------------- PCM format

/// `SLDataFormat_PCM`'s `bitsPerSample`/`containerSize` pair maps onto SPA's
/// separate "packed width" formats rather than a single format plus a
/// padding flag, so this is a lookup rather than arithmetic. Combinations
/// this backend has not needed to describe (20- and 28-bit containers exist
/// in the OpenSL ES spec, but neither Android nor Roblox use them) fail
/// rather than being rounded to the nearest thing that compiles.
bool map_format(uint32_t bits_per_sample, uint32_t container_bits, bool big_endian,
                spa_audio_format& out) {
    if (bits_per_sample == 8 && container_bits == 8) {
        out = SPA_AUDIO_FORMAT_S8;
        return true;
    }
    if (bits_per_sample == 16 && container_bits == 16) {
        out = big_endian ? SPA_AUDIO_FORMAT_S16_BE : SPA_AUDIO_FORMAT_S16_LE;
        return true;
    }
    if (bits_per_sample == 24 && container_bits == 24) {
        out = big_endian ? SPA_AUDIO_FORMAT_S24_BE : SPA_AUDIO_FORMAT_S24_LE;
        return true;
    }
    if (bits_per_sample == 24 && container_bits == 32) {
        out = big_endian ? SPA_AUDIO_FORMAT_S24_32_BE : SPA_AUDIO_FORMAT_S24_32_LE;
        return true;
    }
    if (bits_per_sample == 32 && container_bits == 32) {
        out = big_endian ? SPA_AUDIO_FORMAT_S32_BE : SPA_AUDIO_FORMAT_S32_LE;
        return true;
    }
    return false;
}

} // namespace

/// The same job for AAudio's much smaller format set. `bits`/`is_float`
/// describe one `aaudio_format_t`; `aaudio.cpp` does the enum-to-pair
/// translation so that Android's vocabulary stays in the file that speaks
/// Android, exactly as `DeviceInfo` above keeps it out of this one.
///
/// Little-endian only, and that is not a gap: this backend exists to run
/// Android x86-64 code on a desktop x86-64, and both ends of that are
/// little-endian. A big-endian host would need the whole tree ported first.
bool map_aaudio_format(uint32_t sample_bits, bool is_float, spa_audio_format& out) {
    if (is_float) {
        if (sample_bits == 32) {
            out = SPA_AUDIO_FORMAT_F32_LE;
            return true;
        }
        return false;
    }
    switch (sample_bits) {
    case 16: out = SPA_AUDIO_FORMAT_S16_LE; return true;
    case 24: out = SPA_AUDIO_FORMAT_S24_LE; return true; // three packed bytes, as AAudio's I24_PACKED
    case 32: out = SPA_AUDIO_FORMAT_S32_LE; return true;
    default: return false;
    }
}

/// The inverse, for reporting back what PipeWire actually negotiated.
/// Returns false for a format this bridge cannot describe to the engine,
/// which the caller must treat as a failed open rather than rounding to
/// something nearby — the engine reads these numbers and lays its mixer out
/// against them.
bool describe_format(uint32_t spa_format, uint32_t& sample_bits, bool& is_float) {
    switch (spa_format) {
    case SPA_AUDIO_FORMAT_S16_LE: sample_bits = 16; is_float = false; return true;
    case SPA_AUDIO_FORMAT_S24_LE: sample_bits = 24; is_float = false; return true;
    case SPA_AUDIO_FORMAT_S32_LE: sample_bits = 32; is_float = false; return true;
    case SPA_AUDIO_FORMAT_F32_LE: sample_bits = 32; is_float = true; return true;
    default: return false;
    }
}

// ------------------------------------------------------------------- Impl

using PendingBuffer = testing::PendingBuffer;

namespace testing {

uint32_t fill_pcm(std::deque<PendingBuffer>& pending, uint8_t* dst, uint32_t want,
                   std::vector<void*>& drained_contexts) {
    uint32_t filled = 0;
    while (filled < want && !pending.empty()) {
        PendingBuffer& front = pending.front();
        uint32_t avail = front.size - front.offset;
        uint32_t take = avail < (want - filled) ? avail : (want - filled);
        std::memcpy(dst + filled, front.data + front.offset, take);
        front.offset += take;
        filled += take;
        if (front.offset >= front.size) {
            drained_contexts.push_back(front.context);
            pending.pop_front();
        }
    }
    // Roblox has not kept the queue fed if this is nonzero. Silence, not
    // whatever this buffer held last cycle: pw_stream reuses its buffer
    // pool, so leaving the tail alone would replay stale audio on a loop —
    // which is exactly how a silent gap turns into an audible tone —
    // instead of producing the gap an underrun actually is.
    uint32_t padded = want - filled;
    if (padded != 0) {
        std::memset(dst + filled, 0, padded);
    }
    return padded;
}

} // namespace testing

namespace {

/// `CORDIAL_TRACE_AUDIO=1` prints a running tally every second: buffers
/// enqueued, buffers drained, and how many frames were silence-padded for
/// lack of a fed queue. Off by default — this is a diagnostic for chasing a
/// stalled or underrunning stream, not routine output.
bool trace_audio_enabled() {
    static const bool enabled = std::getenv("CORDIAL_TRACE_AUDIO") != nullptr;
    return enabled;
}

} // namespace

struct PlaybackStream::Impl {
    pw_stream* stream = nullptr;
    uint32_t bytes_per_frame = 0;
    uint32_t max_pending = 2;

    mutable std::mutex mutex;
    std::deque<PendingBuffer> pending;
    PlaybackStream::DrainCallback drain_cb = nullptr;
    void* drain_user = nullptr;
    uint32_t enqueued_index = 0;
    uint64_t underrun_frames = 0; // diagnostic counter, not surfaced through the OpenSL API

    // CORDIAL_TRACE_AUDIO=1 bookkeeping only; untouched otherwise.
    uint64_t trace_process_cycles = 0;
    uint64_t trace_drains = 0;
    std::chrono::steady_clock::time_point trace_last_report{};

    static void on_process(void* data) { static_cast<Impl*>(data)->process(); }

    static void on_state_changed(void* data, pw_stream_state old_state, pw_stream_state state,
                                  const char* error) {
        (void)data;
        (void)old_state;
        // Only the failure transition is worth a line: PAUSED/STREAMING churn
        // as the graph reconfigures (e.g. the default sink changing) and is
        // not, by itself, evidence of anything wrong.
        if (state == PW_STREAM_STATE_ERROR) {
            std::fprintf(stderr,
                "E/Cordial-OpenSLES         PipeWire stream entered the error state (%s).\n",
                error ? error : "no reason given");
        }
    }

    void process() {
        pw_buffer* b = g_lib.stream_dequeue_buffer(stream);
        if (!b) return; // no buffer available this cycle; PipeWire will ask again
        spa_buffer* buf = b->buffer;
        auto* dst = static_cast<uint8_t*>(buf->datas[0].data);
        if (!dst) {
            g_lib.stream_queue_buffer(stream, b);
            return;
        }

        uint32_t capacity = buf->datas[0].maxsize;
        uint32_t want = capacity;
        if (b->requested != 0 && bytes_per_frame != 0) {
            uint64_t requested_bytes = b->requested * static_cast<uint64_t>(bytes_per_frame);
            if (requested_bytes < capacity) want = static_cast<uint32_t>(requested_bytes);
        }

        DrainCallback cb;
        void* user;
        // Contexts of buffers this cycle fully drained. The callback for each
        // fires after `mutex` is released — see the header comment on why:
        // the ordinary OpenSL pattern is to re-enqueue the next buffer from
        // inside this callback, which would deadlock against its own lock.
        std::vector<void*> drained;
        uint32_t padded;
        {
            std::lock_guard<std::mutex> lock(mutex);
            cb = drain_cb;
            user = drain_user;
            padded = testing::fill_pcm(pending, dst, want, drained);
        }
        if (padded != 0) {
            underrun_frames += padded / (bytes_per_frame ? bytes_per_frame : 1);
        }

        buf->datas[0].chunk->offset = 0;
        buf->datas[0].chunk->stride = bytes_per_frame;
        buf->datas[0].chunk->size = want;

        g_lib.stream_queue_buffer(stream, b);

        if (cb) {
            for (void* ctx : drained) cb(ctx, user);
        }

        if (trace_audio_enabled()) {
            ++trace_process_cycles;
            trace_drains += drained.size();
            auto now = std::chrono::steady_clock::now();
            if (now - trace_last_report >= std::chrono::seconds(1)) {
                trace_last_report = now;
                std::fprintf(stderr,
                    "D/Cordial-OpenSLES         audio trace: %llu buffers enqueued, %llu process "
                    "cycles, %llu drained, %llu underrun frames padded with silence\n",
                    static_cast<unsigned long long>(enqueued_index),
                    static_cast<unsigned long long>(trace_process_cycles),
                    static_cast<unsigned long long>(trace_drains),
                    static_cast<unsigned long long>(underrun_frames));
            }
        }
    }

    static const pw_stream_events& events() {
        static const pw_stream_events e = [] {
            pw_stream_events ev{};
            ev.version = PW_VERSION_STREAM_EVENTS;
            ev.state_changed = &Impl::on_state_changed;
            ev.process = &Impl::on_process;
            return ev;
        }();
        return e;
    }
};

// ------------------------------------------------------- device enumeration

namespace {

/// Pulls the `"name"` string out of a `default` metadata value, which
/// WirePlumber writes as the JSON object `{"name":"alsa_output.pci-..."}`.
///
/// Deliberately a scan for one key rather than a JSON parser. SPA ships one
/// (`spa/utils/json.h`) but its API has moved between the versions this file
/// has to compile against, and the alternative — vendoring a parser to read a
/// single string out of a document PipeWire itself generates from a fixed
/// template — is a great deal of surface for no additional truth. An
/// unrecognised shape returns empty, which the caller reads as "no default
/// known" and reports no default at all; that is the honest degradation, and
/// it is visible (no device is marked default) rather than silent.
std::string metadata_value_name(const char* value) {
    if (!value) return {};
    const std::string v(value);
    const std::string key = "\"name\"";
    size_t k = v.find(key);
    if (k == std::string::npos) return {};
    size_t colon = v.find(':', k + key.size());
    if (colon == std::string::npos) return {};
    size_t open_quote = v.find('"', colon + 1);
    if (open_quote == std::string::npos) return {};
    size_t close_quote = v.find('"', open_quote + 1);
    if (close_quote == std::string::npos) return {};
    return v.substr(open_quote + 1, close_quote - open_quote - 1);
}

const char* dict_get(const spa_dict* props, const char* key) {
    if (!props) return nullptr;
    const char* v = spa_dict_lookup(props, key);
    return v;
}

/// What one registry walk collected. Lives on the enumerating thread's stack
/// with the loop locked throughout, so no lock of its own: PipeWire delivers
/// every event below on the loop thread, and the loop thread is blocked
/// inside `round_trip` waiting for us for the whole time these fire.
struct RegistryScan {
    std::vector<DeviceInfo> devices;
    uint32_t default_metadata_id = SPA_ID_INVALID;
    std::string default_sink;
    std::string default_source;
};

} // namespace

std::vector<DeviceInfo> enumerate_devices() {
    Session* session = get_session();
    if (!session) return {};

    RegistryScan scan;

    g_lib.thread_loop_lock(session->loop);

    pw_registry* registry = pw_core_get_registry(session->core, PW_VERSION_REGISTRY, 0);
    if (!registry) {
        g_lib.thread_loop_unlock(session->loop);
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         pw_core_get_registry failed; reporting no audio "
            "devices rather than a guess at what is attached.\n");
        return {};
    }

    pw_registry_events registry_events{};
    registry_events.version = PW_VERSION_REGISTRY_EVENTS;
    registry_events.global = [](void* data, uint32_t id, uint32_t, const char* type, uint32_t,
                                 const spa_dict* props) {
        auto* s = static_cast<RegistryScan*>(data);

        if (type && std::strcmp(type, PW_TYPE_INTERFACE_Metadata) == 0) {
            const char* name = dict_get(props, PW_KEY_METADATA_NAME);
            // There are several metadata objects in a session ("settings",
            // "sm-settings", ...); only the one literally called "default"
            // carries default.audio.sink/source.
            if (name && std::strcmp(name, "default") == 0) s->default_metadata_id = id;
            return;
        }
        if (!type || std::strcmp(type, PW_TYPE_INTERFACE_Node) != 0) return;

        const char* media_class = dict_get(props, PW_KEY_MEDIA_CLASS);
        if (!media_class) return;
        const bool sink = std::strcmp(media_class, "Audio/Sink") == 0;
        const bool source = std::strcmp(media_class, "Audio/Source") == 0;
        // Audio/Duplex, Audio/Source/Virtual, Stream/Output/Audio and the rest
        // are deliberately not reported. A monitor or another application's
        // playback stream is not a device the user would recognise in a picker,
        // and Roblox offering to record from Cordial's own output would be a
        // surprising thing to hand someone.
        if (!sink && !source) return;

        DeviceInfo d;
        d.id = id;
        d.is_source = source;
        if (const char* v = dict_get(props, PW_KEY_NODE_NAME)) d.node_name = v;
        if (const char* v = dict_get(props, PW_KEY_NODE_DESCRIPTION)) d.description = v;
        if (const char* v = dict_get(props, PW_KEY_NODE_NICK)) d.nick = v;
        if (const char* v = dict_get(props, PW_KEY_OBJECT_PATH)) d.object_path = v;
        s->devices.push_back(std::move(d));
    };

    spa_hook registry_listener{};
    pw_registry_add_listener(registry, &registry_listener, &registry_events, &scan);

    // The registry replays every existing global before answering the sync,
    // so one round trip is the whole current session — no polling, and no
    // arbitrary sleep hoping the list has settled.
    const bool listed = round_trip(session);

    // Defaults live in a separate object that has to be bound before it will
    // say anything, which is why this is a second round trip rather than more
    // of the first: the metadata global's id is only known once the registry
    // has announced it above.
    pw_proxy* metadata_proxy = nullptr;
    spa_hook metadata_listener{};
    if (listed && scan.default_metadata_id != SPA_ID_INVALID) {
        metadata_proxy = static_cast<pw_proxy*>(pw_registry_bind(
            registry, scan.default_metadata_id, PW_TYPE_INTERFACE_Metadata, PW_VERSION_METADATA, 0));
        if (metadata_proxy) {
            pw_metadata_events metadata_events{};
            metadata_events.version = PW_VERSION_METADATA_EVENTS;
            metadata_events.property = [](void* data, uint32_t subject, const char* key,
                                           const char*, const char* value) -> int {
                auto* s = static_cast<RegistryScan*>(data);
                if (subject != PW_ID_CORE || !key) return 0;
                if (std::strcmp(key, "default.audio.sink") == 0) {
                    s->default_sink = metadata_value_name(value);
                } else if (std::strcmp(key, "default.audio.source") == 0) {
                    s->default_source = metadata_value_name(value);
                }
                return 0;
            };
            pw_metadata_add_listener(reinterpret_cast<pw_metadata*>(metadata_proxy),
                                     &metadata_listener, &metadata_events, &scan);
            round_trip(session);
        }
    }

    if (metadata_proxy) {
        spa_hook_remove(&metadata_listener);
        g_lib.proxy_destroy(metadata_proxy);
    }
    spa_hook_remove(&registry_listener);
    g_lib.proxy_destroy(reinterpret_cast<pw_proxy*>(registry));

    g_lib.thread_loop_unlock(session->loop);

    if (!listed) {
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         PipeWire did not finish listing its devices; "
            "reporting the %zu found so far rather than a fabricated list.\n",
            scan.devices.size());
    }

    for (DeviceInfo& d : scan.devices) {
        d.is_default = d.is_source ? (!scan.default_source.empty() && d.node_name == scan.default_source)
                                    : (!scan.default_sink.empty() && d.node_name == scan.default_sink);
    }
    return scan.devices;
}

// ------------------------------------------------------- output device choice

namespace testing {

std::string choose_output_target(const std::string& requested,
                                 const std::vector<std::string>& available_sinks,
                                 bool* fell_back) {
    if (fell_back) *fell_back = false;
    // "Follow the default" is not a device that can be missing, so it is not a
    // fallback either. Reporting it as one would put a line in the log on every
    // stream open for the overwhelming majority of users, which is the fastest
    // way to make the line that *does* matter unreadable.
    if (requested.empty()) return {};
    for (const std::string& name : available_sinks) {
        if (name == requested) return requested;
    }
    if (fell_back) *fell_back = true;
    return {};
}

} // namespace testing

const std::string& configured_output_device() {
    static const std::string name = [] {
        const char* v = std::getenv("CORDIAL_AUDIO_SINK");
        std::string s = v ? v : "";
        // A variable set to the empty string is the same instruction as one
        // that is not set: follow the default. `launch.rs` omits it rather
        // than sending an empty value, but a hand-run client with
        // `CORDIAL_AUDIO_SINK=` in its environment must not end up asking for
        // a sink literally called "".
        if (!s.empty()) {
            std::fprintf(stderr,
                "I/Cordial-OpenSLES         output device: playback will be aimed at PipeWire "
                "sink '%s' (CORDIAL_AUDIO_SINK). Unset it to follow the system default.\n",
                s.c_str());
        }
        return s;
    }();
    return name;
}

std::string resolve_output_target(const std::string& requested) {
    if (requested.empty()) return {};

    std::vector<std::string> sinks;
    for (const DeviceInfo& d : enumerate_devices()) {
        if (!d.is_source) sinks.push_back(d.node_name);
    }

    bool fell_back = false;
    std::string target = testing::choose_output_target(requested, sinks, &fell_back);
    if (fell_back) {
        // Loud, and on every open rather than once, because this is the exact
        // shape AGENTS.md forbids a stub from taking: setting
        // PW_KEY_TARGET_OBJECT to a node that is not there leaves PipeWire
        // nothing to link the stream to, and the game then plays perfectly
        // into nowhere. Falling back to the default is the recoverable
        // behaviour; saying nothing about it would make a user's unplugged
        // headset indistinguishable from a Cordial bug.
        std::fprintf(stderr,
            "W/Cordial-OpenSLES         output device '%s' is not in this PipeWire session "
            "(%zu sink(s) present); falling back to the system default so that audio still "
            "plays somewhere. The choice is kept, so replugging the device and relaunching "
            "will use it again.\n",
            requested.c_str(), sinks.size());
    }
    return target;
}

// ---------------------------------------------------------- CallbackStream

/// The AAudio bridge's stream: PipeWire pulls, we pull the engine, in the
/// same call on the same thread. See the class comment in the header for why
/// this has no queue and no mutex where `PlaybackStream` has both.
struct CallbackStream::Impl {
    pw_stream* stream = nullptr;
    Session* session = nullptr;

    CallbackStream::FillCallback fill = nullptr;
    void* user = nullptr;

    /// Written on the loop thread by `on_param_changed`, read by the opening
    /// thread only after `negotiated` has been observed true, and by
    /// `process` on the loop thread thereafter. Not atomic individually
    /// because `negotiated`'s release/acquire pair orders the lot.
    uint32_t rate = 0;
    uint32_t channels = 0;
    uint32_t sample_bytes = 0;
    uint32_t sample_bits = 0;
    bool is_float = false;
    std::atomic<bool> negotiated{false};

    std::atomic<bool> running{false};
    std::atomic<uint32_t> burst{0};
    std::atomic<uint64_t> silence{0};

    static void on_process(void* data) { static_cast<Impl*>(data)->process(); }

    /// **Realtime.** Nothing here locks, allocates, frees, logs or calls into
    /// the kernel; `memset` and the engine's own callback are the whole of
    /// it. That is the property the whole class exists to have — see
    /// `c7215eb` for the deadlock that a mutex on this path produced.
    void process() {
        pw_buffer* b = g_lib.stream_dequeue_buffer(stream);
        if (!b) return; // no buffer this cycle; PipeWire will ask again
        spa_buffer* buf = b->buffer;
        auto* dst = static_cast<uint8_t*>(buf->datas[0].data);
        const uint32_t bpf = channels * sample_bytes;
        if (!dst || bpf == 0) {
            g_lib.stream_queue_buffer(stream, b);
            return;
        }

        uint32_t frames = buf->datas[0].maxsize / bpf;
        if (b->requested != 0 && b->requested < frames) {
            frames = static_cast<uint32_t>(b->requested);
        }
        // One signal per stream, on the first cycle only, so that `open` can
        // stop waiting the moment there is a measured burst size to report.
        // `param_changed` signals for the format; nothing else would signal
        // for this, and without it `open` sits out a full second of
        // `thread_loop_timed_wait` on every stream it brings up. The
        // realtime rule this bends is bent exactly once and never on a cycle
        // that is producing audio.
        if (burst.exchange(frames, std::memory_order_relaxed) == 0 && session) {
            g_lib.thread_loop_signal(session->loop, false);
        }

        bool filled = false;
        if (running.load(std::memory_order_relaxed) && fill != nullptr) {
            filled = fill(dst, frames, user);
            // The callback asking to stop is the AAudio contract's
            // AAUDIO_CALLBACK_RESULT_STOP, not a fault: stop pulling, keep
            // the node connected, and let the owner close it when it likes.
            if (!filled) running.store(false, std::memory_order_relaxed);
        } else if (running.load(std::memory_order_relaxed)) {
            // Running with nothing to pull from. Cannot happen while `fill`
            // is set before connect and never cleared, and counted rather
            // than asserted so that a future change which breaks that shows
            // up as a number instead of as silence nobody can explain.
            silence.fetch_add(1, std::memory_order_relaxed);
        }
        if (!filled) std::memset(dst, 0, static_cast<size_t>(frames) * bpf);

        buf->datas[0].chunk->offset = 0;
        buf->datas[0].chunk->stride = static_cast<int32_t>(bpf);
        buf->datas[0].chunk->size = frames * bpf;

        g_lib.stream_queue_buffer(stream, b);
    }

    /// PipeWire's answer to the deliberately underspecified format `open`
    /// offered. This is the only place rate and channel count are ever set,
    /// and they are read back out of the pod rather than assumed, because
    /// "we asked for nothing in particular and then reported 48000" is
    /// exactly the shape of claim this project keeps having to retract.
    static void on_param_changed(void* data, uint32_t id, const spa_pod* param) {
        auto* self = static_cast<Impl*>(data);
        if (!param || id != SPA_PARAM_Format) return;

        uint32_t media_type = 0, media_subtype = 0;
        if (spa_format_parse(param, &media_type, &media_subtype) < 0) return;
        if (media_type != SPA_MEDIA_TYPE_audio || media_subtype != SPA_MEDIA_SUBTYPE_raw) return;

        spa_audio_info_raw raw{};
        if (spa_format_audio_raw_parse(param, &raw) < 0) return;

        uint32_t bits = 0;
        bool is_float = false;
        if (!describe_format(raw.format, bits, is_float)) return;
        // A Format with no rate or no channel count is not a negotiated
        // format, and treating it as one would have `open` report a rate of
        // zero to an engine that lays its mixer out against the answer.
        if (raw.rate == 0 || raw.channels == 0) return;

        self->rate = raw.rate;
        self->channels = raw.channels;
        self->sample_bits = bits;
        self->sample_bytes = bits / 8;
        self->is_float = is_float;
        self->negotiated.store(true, std::memory_order_release);
        if (self->session) g_lib.thread_loop_signal(self->session->loop, false);
    }

    static void on_state_changed(void* data, pw_stream_state, pw_stream_state state,
                                  const char* error) {
        (void)data;
        if (state == PW_STREAM_STATE_ERROR) {
            std::fprintf(stderr,
                "E/Cordial-AAudio          PipeWire stream entered the error state (%s).\n",
                error ? error : "no reason given");
        }
    }

    static const pw_stream_events& events() {
        static const pw_stream_events e = [] {
            pw_stream_events ev{};
            ev.version = PW_VERSION_STREAM_EVENTS;
            ev.state_changed = &Impl::on_state_changed;
            ev.param_changed = &Impl::on_param_changed;
            ev.process = &Impl::on_process;
            return ev;
        }();
        return e;
    }
};

CallbackStream::CallbackStream() : impl_(new Impl()) {}

CallbackStream::~CallbackStream() {
    close();
    delete impl_;
}

bool CallbackStream::open(uint32_t sample_bits, bool is_float, const char* node_description,
                           const char* target_node_name, FillCallback cb, void* user) {
    if (!cb) return false;
    Session* session = get_session();
    if (!session) return false;

    // `sample_bits == 0` is "no preference", and it must still name a format.
    //
    // The first revision left `wanted` at `SPA_AUDIO_FORMAT_UNKNOWN` in that
    // case, on the theory that an unconstrained EnumFormat lets PipeWire pick
    // everything. It does not: `spa_format_audio_raw_build` omits every zero
    // field, so the pod went out carrying nothing but mediaType and
    // mediaSubtype, and PipeWire never answered with a Format at all. Measured
    // — FMOD asks for `AAUDIO_FORMAT_UNSPECIFIED`, so this was the path every
    // real stream took, and every one of them failed with
    //
    //     PipeWire did not negotiate a format and turn a cycle within 3s
    //     (format no, first cycle no)
    //
    // F32 is the right default and not merely a working one: it is PipeWire's
    // own internal sample format, so choosing it means no conversion anywhere
    // between the engine's mixer and the sink. Rate and channel count are
    // still left at zero, and those two really are filled in by the graph.
    spa_audio_format wanted = SPA_AUDIO_FORMAT_F32_LE;
    if (sample_bits != 0 && !map_aaudio_format(sample_bits, is_float, wanted)) {
        std::fprintf(stderr,
            "E/Cordial-AAudio          no PipeWire format for %u-bit %s samples; refusing to "
            "open a stream rather than substituting a nearby one.\n",
            sample_bits, is_float ? "float" : "integer");
        return false;
    }

    impl_->session = session;
    impl_->fill = cb;
    impl_->user = user;
    impl_->negotiated.store(false, std::memory_order_relaxed);
    impl_->burst.store(0, std::memory_order_relaxed);

    static std::atomic<uint32_t> next_id{0};
    char name[64];
    std::snprintf(name, sizeof name, "cordial-aaudio-%u", next_id.fetch_add(1));

    // Resolved before the loop is locked. `resolve_output_target` enumerates,
    // and enumeration takes the same thread-loop lock; doing it here rather
    // than three lines down is the difference between a device check and a
    // self-deadlock.
    const std::string target =
        resolve_output_target(target_node_name ? std::string(target_node_name) : std::string());

    g_lib.thread_loop_lock(session->loop);

    // Two calls rather than one with a conditional argument: `pw_properties_new`
    // is variadic and terminated by a null, so a null *value* in the middle
    // truncates the list silently. `CaptureStream::open` splits it for the same
    // reason and this matches it deliberately.
    pw_properties* props =
        target.empty()
            ? g_lib.properties_new(
                  PW_KEY_MEDIA_TYPE, "Audio", PW_KEY_MEDIA_CATEGORY, "Playback",
                  PW_KEY_MEDIA_ROLE, "Game", PW_KEY_APP_NAME, "Cordial", PW_KEY_NODE_NAME, name,
                  PW_KEY_NODE_DESCRIPTION,
                  node_description ? node_description : "Cordial (Roblox via AAudio)", nullptr)
            : g_lib.properties_new(
                  PW_KEY_MEDIA_TYPE, "Audio", PW_KEY_MEDIA_CATEGORY, "Playback",
                  PW_KEY_MEDIA_ROLE, "Game", PW_KEY_APP_NAME, "Cordial", PW_KEY_NODE_NAME, name,
                  PW_KEY_NODE_DESCRIPTION,
                  node_description ? node_description : "Cordial (Roblox via AAudio)",
                  PW_KEY_TARGET_OBJECT, target.c_str(), nullptr);

    impl_->stream = g_lib.stream_new_simple(g_lib.thread_loop_get_loop(session->loop), name, props,
                                             &Impl::events(), impl_);
    if (!impl_->stream) {
        std::fprintf(stderr,
            "E/Cordial-AAudio          pw_stream_new_simple failed; no audio.\n");
        g_lib.thread_loop_unlock(session->loop);
        return false;
    }

    // Rate and channel count are left out of the pod on purpose:
    // `spa_format_audio_raw_build` omits any field that is zero, and an
    // omitted field is one PipeWire is free to fill from whatever the sink is
    // already running at. That is the whole of "no resampling on either
    // side", and it is only available because this engine build looks up no
    // AAudioStreamBuilder_setSampleRate to disagree with it.
    uint8_t pod_buffer[1024];
    spa_pod_builder builder = SPA_POD_BUILDER_INIT(pod_buffer, sizeof pod_buffer);
    spa_audio_info_raw info{};
    info.format = wanted;
    info.rate = 0;
    info.channels = 0;
    const spa_pod* params[1];
    params[0] = spa_format_audio_raw_build(&builder, SPA_PARAM_EnumFormat, &info);

    // Connected *active*, unlike `PlaybackStream`. Nothing is audible until
    // `set_running(true)`, because `process` writes silence until then; what
    // this buys is that negotiation and the first cycle happen straight away,
    // so `open` can return measured values for rate, channels and burst
    // instead of placeholders its caller would go on to report to the engine
    // as fact.
    int rc = g_lib.stream_connect(
        impl_->stream, SPA_DIRECTION_OUTPUT, PW_ID_ANY,
        static_cast<pw_stream_flags>(PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS),
        params, 1);

    if (rc < 0) {
        std::fprintf(stderr,
            "E/Cordial-AAudio          pw_stream_connect failed (%s); no audio.\n",
            spa_strerror(rc));
        g_lib.stream_destroy(impl_->stream);
        impl_->stream = nullptr;
        g_lib.thread_loop_unlock(session->loop);
        return false;
    }

    // Three seconds, matching `round_trip` above and for the same reason: a
    // session that cannot negotiate a format and turn one cycle in that long
    // is not one worth making the engine wait on.
    auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(3);
    while ((!impl_->negotiated.load(std::memory_order_acquire) ||
            impl_->burst.load(std::memory_order_relaxed) == 0) &&
           std::chrono::steady_clock::now() < deadline) {
        if (g_lib.thread_loop_timed_wait(session->loop, 1) != 0 &&
            impl_->negotiated.load(std::memory_order_acquire) &&
            impl_->burst.load(std::memory_order_relaxed) != 0) {
            break;
        }
    }

    const bool ready = impl_->negotiated.load(std::memory_order_acquire) &&
                       impl_->burst.load(std::memory_order_relaxed) != 0;
    if (!ready) {
        std::fprintf(stderr,
            "E/Cordial-AAudio          PipeWire did not negotiate a format and turn a cycle "
            "within 3s (format %s, first cycle %s); refusing the stream rather than "
            "reporting a rate nobody agreed to.\n",
            impl_->negotiated.load(std::memory_order_acquire) ? "yes" : "no",
            impl_->burst.load(std::memory_order_relaxed) != 0 ? "yes" : "no");
        g_lib.stream_destroy(impl_->stream);
        impl_->stream = nullptr;
        g_lib.thread_loop_unlock(session->loop);
        return false;
    }

    g_lib.thread_loop_unlock(session->loop);
    return true;
}

void CallbackStream::close() {
    if (!impl_ || !impl_->stream) return;
    impl_->running.store(false, std::memory_order_relaxed);
    Session* session = impl_->session;
    if (session) {
        // The loop lock is the only lock this class ever takes, and it is
        // never held while anything else is. `process` runs on this same loop
        // thread (no PW_STREAM_FLAG_RT_PROCESS), so holding it here excludes
        // the callback outright — which is what makes tearing a stream down
        // underneath a running engine safe without a mutex of our own.
        g_lib.thread_loop_lock(session->loop);
        g_lib.stream_destroy(impl_->stream);
        g_lib.thread_loop_unlock(session->loop);
    }
    impl_->stream = nullptr;
}

bool CallbackStream::is_open() const { return impl_ && impl_->stream != nullptr; }

void CallbackStream::set_running(bool running) {
    if (!impl_) return;
    impl_->running.store(running, std::memory_order_relaxed);
}

bool CallbackStream::is_running() const {
    return impl_ && impl_->running.load(std::memory_order_relaxed);
}

uint32_t CallbackStream::rate_hz() const { return impl_ ? impl_->rate : 0; }
uint32_t CallbackStream::channels() const { return impl_ ? impl_->channels : 0; }
uint32_t CallbackStream::sample_bits() const { return impl_ ? impl_->sample_bits : 0; }
bool CallbackStream::sample_is_float() const { return impl_ && impl_->is_float; }
uint32_t CallbackStream::burst_frames() const {
    return impl_ ? impl_->burst.load(std::memory_order_relaxed) : 0;
}
uint64_t CallbackStream::silence_cycles() const {
    return impl_ ? impl_->silence.load(std::memory_order_relaxed) : 0;
}

// ---------------------------------------------------------- CaptureStream

namespace {

/// Every capture stream currently holding a `pw_stream`. The privacy rule
/// this file is written around is "zero unless Roblox asked to record", and
/// a rule nobody can read the current value of is a rule nobody can check.
std::atomic<uint32_t> g_open_capture_streams{0};

} // namespace

uint32_t active_capture_streams() { return g_open_capture_streams.load(); }

struct CaptureStream::Impl {
    pw_stream* stream = nullptr;
    uint32_t bytes_per_frame = 0;

    mutable std::mutex mutex;
    // A plain byte queue rather than a borrowed-pointer list like the
    // playback side: capture is the other way round — PipeWire owns the
    // memory the samples arrive in and reuses it the moment `process`
    // returns, so anything we intend the reader to see later has to be
    // copied out here, not referenced.
    std::deque<uint8_t> buffered;
    size_t max_buffered = 0;
    uint64_t dropped_bytes = 0;

    static void on_process(void* data) { static_cast<Impl*>(data)->process(); }

    static void on_state_changed(void*, pw_stream_state, pw_stream_state state, const char* error) {
        if (state == PW_STREAM_STATE_ERROR) {
            std::fprintf(stderr,
                "E/Cordial-OpenSLES         PipeWire capture stream entered the error state "
                "(%s); recording will deliver no samples.\n", error ? error : "no reason given");
        }
    }

    void process() {
        pw_buffer* b = g_lib.stream_dequeue_buffer(stream);
        if (!b) return;
        spa_buffer* buf = b->buffer;
        const auto* src = static_cast<const uint8_t*>(buf->datas[0].data);
        if (src) {
            const uint32_t offset = buf->datas[0].chunk->offset;
            const uint32_t size = buf->datas[0].chunk->size;
            std::lock_guard<std::mutex> lock(mutex);
            for (uint32_t i = 0; i < size; ++i) buffered.push_back(src[offset + i]);
            // A reader that has stopped calling `read` must not turn into
            // unbounded memory growth for as long as the stream stays open.
            // Dropping the oldest samples is the right end to drop from: what
            // a late reader wants is the most recent audio, and voice chat has
            // no use for a second-old backlog it would only have to skip.
            while (buffered.size() > max_buffered) {
                buffered.pop_front();
                ++dropped_bytes;
            }
        }
        g_lib.stream_queue_buffer(stream, b);
    }

    static const pw_stream_events& events() {
        static const pw_stream_events e = [] {
            pw_stream_events ev{};
            ev.version = PW_VERSION_STREAM_EVENTS;
            ev.state_changed = &Impl::on_state_changed;
            ev.process = &Impl::on_process;
            return ev;
        }();
        return e;
    }
};

CaptureStream::CaptureStream() : impl_(new Impl()) {}

CaptureStream::~CaptureStream() {
    close();
    delete impl_;
}

bool CaptureStream::open(uint32_t rate_hz, uint32_t channels, const std::string& target_node_name) {
    if (impl_->stream) return true;

    // Cordial has three independent owners of a `CaptureStream`:
    // `AudioRecord` and `WebRtcAudioRecord` in `audio_classes.cpp`, and
    // `AudioRecorderObject` in `opensles.cpp`. Nothing about the engine's own
    // calling pattern rules out two of them recording at once -- Roblox could
    // legitimately have a live voice call open through WebRTC while something
    // else opens the OpenSL recorder that `SL_IID_RECORD` being linked implies
    // exists -- and until review found it, nothing here stopped that: each
    // owner opens its own stream against its own object, with no shared state
    // between the files. Found, not merely unlikely, is the standard
    // `AGENTS.md` sets for "make it impossible rather than unlikely" -- this
    // is that fix, made here rather than divided across the three callers,
    // because this is the one place all three funnel through and the one
    // place the answer can be enforced rather than merely hoped for.
    //
    // A hard refusal rather than a queue or a shared handle: this project has
    // no observed case of Cordial genuinely needing two concurrent captures,
    // real Android's own microphone is a scarcer resource than PipeWire lets
    // it look, and a caller told "no" gets exactly the same honest failure
    // path every other refusal in this file already produces -- see
    // `AGENTS.md`'s rule against a stub that reports success it did not
    // deliver, which a second, silently-degraded capture stream would be a
    // quieter version of.
    if (g_open_capture_streams.load() != 0) {
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         refusing a second capture stream while %u is already "
            "open; Cordial does not support two microphone paths recording at once.\n",
            g_open_capture_streams.load());
        return false;
    }

    Session* session = get_session();
    if (!session) return false;
    if (channels == 0 || channels > SPA_AUDIO_MAX_CHANNELS || rate_hz == 0) {
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         refusing to open a capture stream at %u Hz / %u "
            "channels; nothing will be recorded.\n", rate_hz, channels);
        return false;
    }

    impl_->bytes_per_frame = channels * 2; // S16 only; see the format param below
    // Half a second. Long enough to absorb a reader that misses its slot
    // while the graph re-negotiates, short enough that the samples a reader
    // eventually sees are still worth hearing.
    impl_->max_buffered = static_cast<size_t>(impl_->bytes_per_frame) * rate_hz / 2;
    {
        std::lock_guard<std::mutex> lock(impl_->mutex);
        impl_->buffered.clear();
        impl_->dropped_bytes = 0;
    }

    static std::atomic<uint32_t> next_id{0};
    char name[64];
    std::snprintf(name, sizeof name, "cordial-audiorecord-%u", next_id.fetch_add(1));

    g_lib.thread_loop_lock(session->loop);

    // PW_KEY_MEDIA_ROLE "Communication" is not decoration: it is what tells
    // the session manager this is a voice stream, which is what makes the
    // desktop's own microphone indicator light up and name Cordial. Being
    // conspicuous while recording is the other half of the promise not to
    // record when unasked.
    pw_properties* props =
        target_node_name.empty()
            ? g_lib.properties_new(PW_KEY_MEDIA_TYPE, "Audio", PW_KEY_MEDIA_CATEGORY, "Capture",
                                   PW_KEY_MEDIA_ROLE, "Communication", PW_KEY_APP_NAME, "Cordial",
                                   PW_KEY_NODE_NAME, name, PW_KEY_NODE_DESCRIPTION,
                                   "Cordial (Roblox voice chat)", nullptr)
            : g_lib.properties_new(PW_KEY_MEDIA_TYPE, "Audio", PW_KEY_MEDIA_CATEGORY, "Capture",
                                   PW_KEY_MEDIA_ROLE, "Communication", PW_KEY_APP_NAME, "Cordial",
                                   PW_KEY_NODE_NAME, name, PW_KEY_NODE_DESCRIPTION,
                                   "Cordial (Roblox voice chat)", PW_KEY_TARGET_OBJECT,
                                   target_node_name.c_str(), nullptr);

    impl_->stream = g_lib.stream_new_simple(g_lib.thread_loop_get_loop(session->loop), name, props,
                                             &Impl::events(), impl_);
    if (!impl_->stream) {
        g_lib.thread_loop_unlock(session->loop);
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         pw_stream_new_simple failed for capture; nothing "
            "will be recorded.\n");
        return false;
    }

    uint8_t pod_buffer[1024];
    spa_pod_builder builder = SPA_POD_BUILDER_INIT(pod_buffer, sizeof pod_buffer);
    spa_audio_info_raw info{};
    // S16 native-endian, because that is the only thing either caller wants:
    // android.media.AudioRecord's ENCODING_PCM_16BIT and WebRTC's voice
    // engine both deal exclusively in signed 16-bit interleaved frames.
    info.format = SPA_AUDIO_FORMAT_S16;
    info.channels = channels;
    info.rate = rate_hz;
    if (channels == 1) {
        info.position[0] = SPA_AUDIO_CHANNEL_MONO;
    } else if (channels == 2) {
        info.position[0] = SPA_AUDIO_CHANNEL_FL;
        info.position[1] = SPA_AUDIO_CHANNEL_FR;
    } else {
        info.flags |= SPA_AUDIO_FLAG_UNPOSITIONED;
    }

    const spa_pod* params[1];
    params[0] = spa_format_audio_raw_build(&builder, SPA_PARAM_EnumFormat, &info);

    // Connected active, unlike playback: there is no separate "start" step on
    // this path. `open` is only ever reached from the engine asking to record,
    // so an inactive capture stream would be a microphone held open doing
    // nothing — precisely the state this class exists to make impossible.
    int rc = g_lib.stream_connect(
        impl_->stream, SPA_DIRECTION_INPUT, PW_ID_ANY,
        static_cast<pw_stream_flags>(PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS),
        params, 1);

    g_lib.thread_loop_unlock(session->loop);

    if (rc < 0) {
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         pw_stream_connect failed for capture (%s); nothing "
            "will be recorded.\n", spa_strerror(rc));
        g_lib.thread_loop_lock(session->loop);
        g_lib.stream_destroy(impl_->stream);
        g_lib.thread_loop_unlock(session->loop);
        impl_->stream = nullptr;
        return false;
    }

    g_open_capture_streams.fetch_add(1);
    std::fprintf(stderr,
        "I/Cordial-OpenSLES         microphone opened: %u Hz, %u channel(s), source '%s'. "
        "%u capture stream(s) now open.\n",
        rate_hz, channels, target_node_name.empty() ? "(PipeWire default)" : target_node_name.c_str(),
        g_open_capture_streams.load());
    return true;
}

void CaptureStream::close() {
    if (!impl_ || !impl_->stream) return;
    Session* session = get_session();
    if (session) {
        g_lib.thread_loop_lock(session->loop);
        // Destroyed, not deactivated. `pw_stream_set_active(false)` leaves the
        // node in the graph, which leaves the desktop's microphone indicator
        // lit and leaves every other application seeing Cordial holding the
        // capture device. Nothing short of destroying the stream makes a
        // stopped recording indistinguishable from one that never started.
        g_lib.stream_destroy(impl_->stream);
        g_lib.thread_loop_unlock(session->loop);
    }
    impl_->stream = nullptr;
    {
        std::lock_guard<std::mutex> lock(impl_->mutex);
        impl_->buffered.clear();
    }
    uint32_t remaining = g_open_capture_streams.fetch_sub(1) - 1;
    std::fprintf(stderr,
        "I/Cordial-OpenSLES         microphone closed; %u capture stream(s) now open.\n",
        remaining);
}

bool CaptureStream::is_open() const { return impl_ && impl_->stream != nullptr; }

uint32_t CaptureStream::read(void* dst, uint32_t size) {
    if (!impl_ || !dst || size == 0) return 0;
    std::lock_guard<std::mutex> lock(impl_->mutex);
    uint32_t n = static_cast<uint32_t>(impl_->buffered.size() < size ? impl_->buffered.size() : size);
    auto* out = static_cast<uint8_t*>(dst);
    for (uint32_t i = 0; i < n; ++i) {
        out[i] = impl_->buffered.front();
        impl_->buffered.pop_front();
    }
    return n;
}

uint64_t CaptureStream::dropped_bytes() const {
    if (!impl_) return 0;
    std::lock_guard<std::mutex> lock(impl_->mutex);
    return impl_->dropped_bytes;
}

// --------------------------------------------------------------- interface

bool pipewire_available() { return get_session() != nullptr; }

PlaybackStream::PlaybackStream() : impl_(new Impl()) {}

PlaybackStream::~PlaybackStream() {
    close();
    delete impl_;
}

bool PlaybackStream::open(uint32_t rate_hz, uint32_t channels, uint32_t bits_per_sample,
                           uint32_t container_bits, bool big_endian, uint32_t max_pending_buffers,
                           const std::string& target_node_name) {
    Session* session = get_session();
    if (!session) return false; // slCreateEngine already refused if this is reachable; defensive only

    spa_audio_format format;
    if (!map_format(bits_per_sample, container_bits, big_endian, format)) {
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         unsupported PCM layout: %u-bit samples in a "
            "%u-bit container; no audio for this player.\n", bits_per_sample, container_bits);
        return false;
    }
    if (channels == 0 || channels > SPA_AUDIO_MAX_CHANNELS) {
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         unsupported channel count %u; no audio for this "
            "player.\n", channels);
        return false;
    }

    impl_->bytes_per_frame = channels * (container_bits / 8);
    impl_->max_pending = max_pending_buffers > 0 ? max_pending_buffers : 2;

    static std::atomic<uint32_t> next_id{0};
    char name[64];
    std::snprintf(name, sizeof name, "cordial-audioplayer-%u", next_id.fetch_add(1));

    // Before the lock: see the same call in `CallbackStream::open`.
    const std::string target = resolve_output_target(target_node_name);

    g_lib.thread_loop_lock(session->loop);

    pw_properties* props =
        target.empty()
            ? g_lib.properties_new(PW_KEY_MEDIA_TYPE, "Audio", PW_KEY_MEDIA_CATEGORY, "Playback",
                                   PW_KEY_MEDIA_ROLE, "Game", PW_KEY_APP_NAME, "Cordial",
                                   PW_KEY_NODE_NAME, name, PW_KEY_NODE_DESCRIPTION,
                                   "Cordial (Roblox via OpenSL ES)", nullptr)
            : g_lib.properties_new(PW_KEY_MEDIA_TYPE, "Audio", PW_KEY_MEDIA_CATEGORY, "Playback",
                                   PW_KEY_MEDIA_ROLE, "Game", PW_KEY_APP_NAME, "Cordial",
                                   PW_KEY_NODE_NAME, name, PW_KEY_NODE_DESCRIPTION,
                                   "Cordial (Roblox via OpenSL ES)", PW_KEY_TARGET_OBJECT,
                                   target.c_str(), nullptr);

    impl_->stream = g_lib.stream_new_simple(g_lib.thread_loop_get_loop(session->loop), name, props,
                                             &Impl::events(), impl_);
    if (!impl_->stream) {
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         pw_stream_new_simple failed; no audio for this "
            "player.\n");
        g_lib.thread_loop_unlock(session->loop);
        return false;
    }

    uint8_t pod_buffer[1024];
    spa_pod_builder builder = SPA_POD_BUILDER_INIT(pod_buffer, sizeof pod_buffer);
    spa_audio_info_raw info{};
    info.format = format;
    info.channels = channels;
    info.rate = rate_hz;
    if (channels == 1) {
        info.position[0] = SPA_AUDIO_CHANNEL_MONO;
    } else if (channels == 2) {
        info.position[0] = SPA_AUDIO_CHANNEL_FL;
        info.position[1] = SPA_AUDIO_CHANNEL_FR;
    } else {
        info.flags |= SPA_AUDIO_FLAG_UNPOSITIONED;
    }

    const spa_pod* params[1];
    params[0] = spa_format_audio_raw_build(&builder, SPA_PARAM_EnumFormat, &info);

    // Connected inactive: Realize brings the resource into existence, but
    // playback only starts once SLPlayItf::SetPlayState asks for
    // SL_PLAYSTATE_PLAYING, same as the object model's own state machine.
    int rc = g_lib.stream_connect(
        impl_->stream, SPA_DIRECTION_OUTPUT, PW_ID_ANY,
        static_cast<pw_stream_flags>(PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS |
                                      PW_STREAM_FLAG_INACTIVE),
        params, 1);

    g_lib.thread_loop_unlock(session->loop);

    if (rc < 0) {
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         pw_stream_connect failed (%s); no audio for this "
            "player.\n", spa_strerror(rc));
        g_lib.thread_loop_lock(session->loop);
        g_lib.stream_destroy(impl_->stream);
        g_lib.thread_loop_unlock(session->loop);
        impl_->stream = nullptr;
        return false;
    }
    return true;
}

void PlaybackStream::close() {
    if (!impl_->stream) return;
    Session* session = get_session();
    if (session) {
        g_lib.thread_loop_lock(session->loop);
        g_lib.stream_destroy(impl_->stream);
        g_lib.thread_loop_unlock(session->loop);
    }
    impl_->stream = nullptr;
}

bool PlaybackStream::enqueue(const void* data, uint32_t size, void* buffer_context) {
    if (size == 0) return true;
    std::lock_guard<std::mutex> lock(impl_->mutex);
    if (impl_->pending.size() >= impl_->max_pending) return false;
    impl_->pending.push_back({static_cast<const uint8_t*>(data), size, 0, buffer_context});
    ++impl_->enqueued_index;
    return true;
}

void PlaybackStream::clear() {
    std::lock_guard<std::mutex> lock(impl_->mutex);
    impl_->pending.clear();
}

void PlaybackStream::set_drain_callback(DrainCallback cb, void* user) {
    std::lock_guard<std::mutex> lock(impl_->mutex);
    impl_->drain_cb = cb;
    impl_->drain_user = user;
}

void PlaybackStream::set_active(bool active) {
    if (!impl_->stream) return;
    Session* session = get_session();
    if (!session) return;
    g_lib.thread_loop_lock(session->loop);
    g_lib.stream_set_active(impl_->stream, active);
    g_lib.thread_loop_unlock(session->loop);
}

void PlaybackStream::set_volume_linear(float linear) {
    if (!impl_->stream) return;
    Session* session = get_session();
    if (!session) return;

    uint8_t pod_buffer[128];
    spa_pod_builder builder = SPA_POD_BUILDER_INIT(pod_buffer, sizeof pod_buffer);
    const spa_pod* params[1];
    params[0] = static_cast<const spa_pod*>(spa_pod_builder_add_object(
        &builder, SPA_TYPE_OBJECT_Props, SPA_PARAM_Props,
        SPA_PROP_volume, SPA_POD_Float(linear)));

    g_lib.thread_loop_lock(session->loop);
    g_lib.stream_update_params(impl_->stream, params, 1);
    g_lib.thread_loop_unlock(session->loop);
}

void PlaybackStream::set_mute(bool mute) {
    if (!impl_->stream) return;
    Session* session = get_session();
    if (!session) return;

    uint8_t pod_buffer[128];
    spa_pod_builder builder = SPA_POD_BUILDER_INIT(pod_buffer, sizeof pod_buffer);
    const spa_pod* params[1];
    params[0] = static_cast<const spa_pod*>(spa_pod_builder_add_object(
        &builder, SPA_TYPE_OBJECT_Props, SPA_PARAM_Props,
        SPA_PROP_mute, SPA_POD_Bool(mute)));

    g_lib.thread_loop_lock(session->loop);
    g_lib.stream_update_params(impl_->stream, params, 1);
    g_lib.thread_loop_unlock(session->loop);
}

PlaybackStream::QueueState PlaybackStream::state() const {
    std::lock_guard<std::mutex> lock(impl_->mutex);
    return {static_cast<uint32_t>(impl_->pending.size()), impl_->enqueued_index};
}

} // namespace cordial::audio

#else // !CORDIAL_HAVE_PIPEWIRE

// No PipeWire headers at configure time (see CMakeLists.txt). Every entry
// point degrades to "no audio", the same answer this backend gives for an
// absent library or an absent session at run time — from `opensles.cpp`'s
// side these three cases are indistinguishable and deliberately so.

#include <cstdio>
#include <cstdlib>

namespace cordial::audio {

bool pipewire_available() {
    static const bool warned = [] {
        std::fprintf(stderr,
            "E/Cordial-Audio           built without pipewire-devel present at configure "
            "time (see native/CMakeLists.txt); there is no audio at all, by either the "
            "OpenSL ES or the AAudio path -- both reach PipeWire through this. Install "
            "pipewire-devel and reconfigure to enable it.\n");
        return true;
    }();
    (void)warned;
    return false;
}

/// No session, so no devices. Empty rather than one invented "Default"
/// entry: a device picker listing something that cannot play is worse than
/// one that is honestly empty, because only the empty one sends the user
/// looking for the real problem.
std::vector<DeviceInfo> enumerate_devices() { return {}; }

uint32_t active_capture_streams() { return 0; }

/// The user's choice is still read and still reported, even though nothing
/// here can act on it. Returning empty instead would make a build without
/// pipewire-devel report "following the system default" to anyone who asked,
/// which is a different and untrue thing from "there is no audio at all".
const std::string& configured_output_device() {
    static const std::string name = [] {
        const char* v = std::getenv("CORDIAL_AUDIO_SINK");
        return std::string(v ? v : "");
    }();
    return name;
}

std::string resolve_output_target(const std::string&) { return {}; }

namespace testing {

/// Shares no code with the real one on purpose: it is four lines, and the
/// alternative is hoisting it into the header where it would be the only
/// logic in a file that is otherwise declarations.
std::string choose_output_target(const std::string& requested,
                                 const std::vector<std::string>& available_sinks,
                                 bool* fell_back) {
    if (fell_back) *fell_back = false;
    if (requested.empty()) return {};
    for (const std::string& name : available_sinks) {
        if (name == requested) return requested;
    }
    if (fell_back) *fell_back = true;
    return {};
}

} // namespace testing

struct CaptureStream::Impl {};

CaptureStream::CaptureStream() : impl_(nullptr) {}
CaptureStream::~CaptureStream() {}

bool CaptureStream::open(uint32_t, uint32_t, const std::string&) { return false; }
void CaptureStream::close() {}
bool CaptureStream::is_open() const { return false; }
uint32_t CaptureStream::read(void*, uint32_t) { return 0; }
uint64_t CaptureStream::dropped_bytes() const { return 0; }

struct CallbackStream::Impl {};

CallbackStream::CallbackStream() : impl_(nullptr) {}
CallbackStream::~CallbackStream() {}

bool CallbackStream::open(uint32_t, bool, const char*, const char*, FillCallback, void*) {
    return false;
}
void CallbackStream::close() {}
bool CallbackStream::is_open() const { return false; }
void CallbackStream::set_running(bool) {}
bool CallbackStream::is_running() const { return false; }
uint32_t CallbackStream::rate_hz() const { return 0; }
uint32_t CallbackStream::channels() const { return 0; }
uint32_t CallbackStream::sample_bits() const { return 0; }
bool CallbackStream::sample_is_float() const { return false; }
uint32_t CallbackStream::burst_frames() const { return 0; }
uint64_t CallbackStream::silence_cycles() const { return 0; }

struct PlaybackStream::Impl {};

PlaybackStream::PlaybackStream() : impl_(nullptr) {}
PlaybackStream::~PlaybackStream() {}

bool PlaybackStream::open(uint32_t, uint32_t, uint32_t, uint32_t, bool, uint32_t,
                           const std::string&) {
    return false;
}
void PlaybackStream::close() {}
bool PlaybackStream::enqueue(const void*, uint32_t, void*) { return false; }
void PlaybackStream::clear() {}
void PlaybackStream::set_drain_callback(DrainCallback, void*) {}
void PlaybackStream::set_active(bool) {}
void PlaybackStream::set_volume_linear(float) {}
void PlaybackStream::set_mute(bool) {}
PlaybackStream::QueueState PlaybackStream::state() const { return {0, 0}; }

} // namespace cordial::audio

#endif // CORDIAL_HAVE_PIPEWIRE

// ---------------------------------------------------------------- the seam
//
// Outside the `#ifdef`, because both arms of it define `CallbackStream` and
// both are legitimate answers to "what backend is there". A tree built without
// pipewire-devel gets a `CallbackStream` whose `open` fails honestly, which is
// exactly what a host with no backend at all should hand back.

namespace cordial::audio {

const char* host_backend_name() {
    // One reader, and it caches. `aaudio.cpp`'s own `CORDIAL_AUDIO` parser
    // documents the same rule for the same reason: two readers of one variable
    // is two places for a typo to mean different things.
    static const char* const name = [] () -> const char* {
        const char* raw = std::getenv("CORDIAL_AUDIO_HOST");
        // **Unset means detect, not PipeWire.** It used to mean PipeWire, so a
        // machine without it was told "no audio" while ALSA and OSS sat
        // unexamined a few lines below -- reported by a user on 2026-08-27 who
        // reasonably concluded the variable did nothing. Nobody with a PipeWire
        // session sees a difference: detection asks for it first.
        if (!raw || raw[0] == '\0') return "auto";
        // Lowercased before comparing. `CORDIAL_AUDIO_HOST=OSS` used to fall
        // through to the warning and silently become PipeWire, which is a sharp
        // edge on a variable somebody only ever types when audio is already
        // broken.
        static std::string folded;
        folded.assign(raw);
        for (char& c : folded) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
        const char* value = folded.c_str();
        if (std::strcmp(value, "pipewire") == 0) return "pipewire";
        if (std::strcmp(value, "pulse") == 0 || std::strcmp(value, "pulseaudio") == 0) {
            return "pulse";
        }
        if (std::strcmp(value, "alsa") == 0) return "alsa";
        if (std::strcmp(value, "oss") == 0) return "oss";
        if (std::strcmp(value, "auto") == 0) return "auto";
        // Falls back rather than failing, and says so. ADR-023 schedules
        // PulseAudio and then ALSA behind this name; until one of them exists,
        // a run that asked for one should be told it did not get it rather
        // than left to infer it from silence.
        std::fprintf(stderr,
            "W/Cordial-Audio           CORDIAL_AUDIO_HOST=%s is not a backend this build has "
            "(pipewire, pulse, alsa, oss); using pipewire. See "
            "docs/adr/ADR-023-host-audio-backends.md.\n",
            value);
        return "pipewire";
    }();
    return name;
}

/// The backend this run will actually use, resolved once.
///
/// **This is what makes "detect" mean anything.** `host_backend_name()` reports
/// what was *asked for*; this reports what is *there*, by probing in the order
/// ADR-023 sets out -- and the probes are real: `oss_available()` opens
/// `/dev/dsp`, `pulse_available()` connects to a server and tears the
/// connection down, `pipewire_available()` waits for a session to answer. A
/// "no" here is a measured absence rather than a guess about the machine.
///
/// PipeWire first, so nobody who has one sees any change. The order after it is
/// PulseAudio, ALSA, OSS -- most to least likely on a desktop, and OSS last
/// because the machines that have it are the ones that have nothing else.
///
/// Cached, because these probes cost a connection attempt each and the answer
/// cannot change while the process runs.
const char* effective_backend_name() {
    static const char* const chosen = [] () -> const char* {
        const char* asked = host_backend_name();
        if (std::strcmp(asked, "auto") != 0) {
            // Somebody named one. `make_output_stream` already announces a
            // fallback if it will not open, and being told what you asked for
            // is what lets that message make sense.
            return asked;
        }
        if (pipewire_available()) return "pipewire";
        if (pulse_available()) return "pulse";
        if (alsa_available()) return "alsa";
        if (oss_available()) return "oss";
        // Nothing answered. PipeWire is returned so the existing path runs and
        // prints its own diagnosis, which names the library and what to install
        // -- a better message than anything that could be invented here.
        return "pipewire";
    }();
    return chosen;
}

/// Whether audio can work at all on this host.
///
/// The predicate the one-way doors must ask. They asked `pipewire_available()`,
/// which is the question "is PipeWire here" rather than "can we play sound", so
/// on an OSS-only machine `supportsAAudio()` answered false, FMOD never loaded
/// the AAudio path, and the selector that would have chosen OSS was never
/// reached. ADR-023 says this predicate must be consulted "before the door
/// closes"; it never was.
bool host_backend_available() {
    const char* name = effective_backend_name();
    if (std::strcmp(name, "oss") == 0) return oss_available();
    if (std::strcmp(name, "alsa") == 0) return alsa_available();
    if (std::strcmp(name, "pulse") == 0) return pulse_available();
    return pipewire_available();
}

std::unique_ptr<OutputStream> make_output_stream() {
    // The whole point of ADR-023's first step: the caller stopped naming a
    // backend, so adding one is a change here and nowhere else.
    //
    // **PulseAudio is opt-in and PipeWire stays the default**, which is the
    // decision rather than caution about the code. Every current user's session
    // runs PipeWire, `pipewire-pulse` already serves libpulse clients on it, and
    // a default that changed under them would be a change nobody asked for. The
    // people this exists for are on a machine where PipeWire is not there --
    // and they are the ones who will set the variable.
    if (std::strcmp(effective_backend_name(), "oss") == 0) {
        if (oss_available()) {
            if (auto stream = make_oss_stream()) return stream;
        }
        std::fprintf(stderr,
            "W/Cordial-Audio           CORDIAL_AUDIO_HOST=oss, but /dev/dsp would not open "
            "(set CORDIAL_AUDIO_DEVICE for another node); using pipewire.\n");
    }
    if (std::strcmp(effective_backend_name(), "alsa") == 0) {
        if (alsa_available()) {
            if (auto stream = make_alsa_stream()) return stream;
        }
        std::fprintf(stderr,
            "W/Cordial-Audio           CORDIAL_AUDIO_HOST=alsa, but no ALSA device would "
            "open; using pipewire.\n");
    }
    if (std::strcmp(effective_backend_name(), "pulse") == 0) {
        // Asked before falling back, not after: `pulse_available()` connects to
        // a server and tears the connection down, so a "no" here is a measured
        // absence rather than a guess -- and the fallback is announced, because
        // silently getting a different backend from the one you asked for is
        // the failure this whole file is arranged to avoid.
        if (pulse_available()) {
            if (auto stream = make_pulse_stream()) return stream;
        }
        std::fprintf(stderr,
            "W/Cordial-Audio           CORDIAL_AUDIO_HOST=pulse, but no PulseAudio server "
            "answered; using pipewire.\n");
    }
    return std::make_unique<CallbackStream>();
}

} // namespace cordial::audio


// The includes are repeated here rather than hoisted to the top of the file:
// everything above this line is inside one arm of the `#ifdef` or the other,
// and this block is in neither.
#include <cstdlib>
#include <cstring>
#include <vector>

// ------------------------------------------------------- the picker's window
//
// Outside both branches of the `#ifdef`, because both define
// `enumerate_devices` and this is only a projection of it. A build without
// pipewire-devel therefore still exports these symbols and still answers
// honestly — zero sinks — rather than failing to link the shell that calls
// them.

extern "C" {

size_t cordial_audio_sinks(CordialAudioSink** out) {
    if (!out) return 0;
    *out = nullptr;

    std::vector<cordial::audio::DeviceInfo> devices = cordial::audio::enumerate_devices();

    size_t count = 0;
    for (const cordial::audio::DeviceInfo& d : devices) {
        if (!d.is_source && !d.node_name.empty()) ++count;
    }
    if (count == 0) return 0;

    auto* list = static_cast<CordialAudioSink*>(std::calloc(count, sizeof(CordialAudioSink)));
    if (!list) return 0;

    size_t i = 0;
    for (const cordial::audio::DeviceInfo& d : devices) {
        if (d.is_source || d.node_name.empty()) continue;
        // `strdup` rather than handing out pointers into the vector, which
        // dies at the closing brace. Each string is freed individually below.
        list[i].node_name = ::strdup(d.node_name.c_str());
        // A sink with no `node.description` is unusual and not impossible —
        // a hand-written null-sink in a config file has none. Showing the
        // routing name is worse than showing a description and much better
        // than showing an empty row the user cannot tell apart from the next
        // empty row.
        list[i].description =
            ::strdup(d.description.empty() ? d.node_name.c_str() : d.description.c_str());
        list[i].is_default = d.is_default ? 1 : 0;
        ++i;
    }
    *out = list;
    return count;
}

void cordial_audio_sinks_free(CordialAudioSink* sinks, size_t count) {
    if (!sinks) return;
    for (size_t i = 0; i < count; ++i) {
        std::free(const_cast<char*>(sinks[i].node_name));
        std::free(const_cast<char*>(sinks[i].description));
    }
    std::free(sinks);
}

} // extern "C"
