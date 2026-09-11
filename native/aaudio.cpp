// AAudio, backed by PipeWire — Android's callback-driven audio output, which
// is the one FMOD prefers on any modern Android and the one Roblox therefore
// asks for first.
//
// Why this exists rather than leaving well alone. Audio currently reaches
// PipeWire through FMOD's *Java* fallback, `org/fmod/AudioDevice`, over JNI
// and jnivm: the engine hands `write([BI)V` a `jbyteArray` per block and
// `native/audio_classes.cpp` copies it into `PlaybackStream`'s pending queue.
// That layer is where the freeze fixed in `c7215eb` lived — an AB-BA deadlock
// between `AudioDevice::close`, holding its own `std::mutex` while waiting on
// PipeWire's thread-loop lock, and PipeWire's own thread, holding that lock
// while waiting on the same mutex inside `AudioDevice::drained`. It took two
// days to find because the symptom was a client that looked wedged in its
// renderer.
//
// AAudio's data callback is the same shape as PipeWire's `process()`: "here
// is a buffer, fill it, you are on a realtime thread". Bridging them deletes
// the queue, the copy, the JNI hop and the mutex rather than making any of
// them safe, so that class of bug becomes structurally impossible instead of
// avoided by discipline. `CallbackStream` in `pipewire_backend.cpp` is the
// half of it that speaks PipeWire; this file is the half that speaks Android.
//
// **Once FMOD has been told AAudio exists, there is no way back.** This is
// the most consequential thing measured while writing this file, and it is
// the reverse of what the plan assumed. A control run
// (`CORDIAL_AUDIO=aaudio-refuse`) answered `supportsAAudio()` true and then
// reported `AAUDIO_ERROR_UNAVAILABLE` from every `openStream`. FMOD tried
// twice and then abandoned audio altogether: no `AudioDevice.init`, no
// `slCreateEngine`, no PipeWire node of any kind for the rest of the run, and
// a place that loaded and played in silence. The "AAudio, then OpenSL ES,
// then Java" chain does not exist on this build once the first link has been
// claimed. That is why `supportsAAudio()` checks `pipewire_available()`
// before saying yes, and why this whole path stays behind a switch that is
// off by default.
//
// **What the engine actually asks for is measured, not assumed.**
// `docs/analysis/aaudio-contract.md` lists the 25 `AAudio*` names present in
// `libroblox.so` 2.734.0.917, and the absences shaped this file more than the
// presences did:
//
//   * There is no `AAudioStream_write`, only `_read`. Playback is
//     callback-driven and nothing else, so there is no write path here to go
//     stale.
//   * There is no `setSampleRate` and no `setChannelCount`. The engine opens
//     a stream and then reads `getSampleRate`/`getChannelCount`/`getFormat`
//     back off it. So `CallbackStream::open` constrains neither, PipeWire
//     picks whatever the session is already running at, and these getters
//     report that. Nothing resamples at either end.
//   * There is no `setDeviceId`, so nothing on this path can select an
//     output. Roblox's own device picker is fed by `AudioManager.getDevices`
//     in `audio_classes.cpp` and is a separate job.
//
// Every type, constant and prototype below is transcribed from AOSP's public
// `media/libaaudio/include/aaudio/AAudio.h` (Apache-2.0,
// android.googlesource.com), the same provenance and the same reason as the
// Khronos and AOSP headers `native/opensles.cpp` names at its own top: an
// ABI recalled from memory is an ABI that silently plays noise. The header is
// not vendored — only the handful of values this file uses are written out,
// so the tree gains no new build-time dependency.
//
// **Input used to be refused here and now is not.** The first revision of
// this file reported `AAUDIO_ERROR_UNIMPLEMENTED` for every
// `AAUDIO_DIRECTION_INPUT` open, on the grounds that the privacy rule the
// audio code is written around needs care a class designed to be pulled
// forever does not give for free. That was the right call while capture was
// unwritten and it is the wrong one now: FMOD does not fall back, so a
// refused input stream on a build that has committed to AAudio is a
// microphone that cannot work at all.
//
// Capture is therefore implemented, and it reuses `CaptureStream` — the same
// class `android.media.AudioRecord` and `SLRecordItf` already record through,
// with the same lifetime rule and the same `pw_stream`-destroying `close()`.
// The one thing this file has to get right is *when* that class is asked to
// open, and the answer is `AAudioStream_requestStart` and nowhere else:
//
//   * `openStream` on an input stream allocates a `Stream` and no PipeWire
//     resource. An AAudio stream that has been opened and not started is not
//     recording, so there is nothing in the registry to find.
//   * `requestStart` opens the capture stream. `requestPause`, `requestStop`
//     and `close` all destroy it — pause included, because AAudio's "keep the
//     stream, stop consuming" is exactly the paused-but-present state the
//     rule at the top of `audio_classes.cpp` refuses to allow.
//   * Every failure path closes. A `requestStart` whose open fails stays
//     STOPPED with nothing held, and `~Stream` closes again behind it.
//
// The shape is a *blocking read* rather than a callback, and that is measured
// rather than chosen: `AAudioStream_read` is in the engine's 25 symbols and
// `AAudioStream_write` is not, so the engine pulls capture on a thread of its
// own. An input stream that arrives carrying a data callback is refused,
// loudly, because feeding one would need a thread of ours with the microphone
// tied to its lifetime rather than to `requestStart` — see AGENTS.md on stubs
// that lie, and note that a stream opened for a callback nobody calls is
// precisely a microphone held open for a recording that is not happening.

#include "aaudio.h"
#include "pipewire_backend.h"

#include <memory>

#include <pthread.h>
#include "os_compat.h"

#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <new>
#include <string>

namespace {

// ------------------------------------------------------------ the AAudio ABI
//
// From AOSP's AAudio.h. The enumerators that matter are spelled out with
// their values rather than left to sequence position, because a wrong number
// here is not a compile error anywhere — it is audio that is quietly the
// wrong format, or a result code the engine reads as success.

using aaudio_result_t = int32_t;
using aaudio_stream_state_t = int32_t;
using aaudio_direction_t = int32_t;
using aaudio_format_t = int32_t;
using aaudio_data_callback_result_t = int32_t;
using aaudio_performance_mode_t = int32_t;
using aaudio_usage_t = int32_t;
using aaudio_input_preset_t = int32_t;

constexpr aaudio_result_t AAUDIO_OK = 0;
constexpr aaudio_result_t AAUDIO_ERROR_ILLEGAL_ARGUMENT = -898;
constexpr aaudio_result_t AAUDIO_ERROR_INVALID_STATE = -895;
constexpr aaudio_result_t AAUDIO_ERROR_UNIMPLEMENTED = -890;
constexpr aaudio_result_t AAUDIO_ERROR_UNAVAILABLE = -889;
constexpr aaudio_result_t AAUDIO_ERROR_NO_MEMORY = -887;
constexpr aaudio_result_t AAUDIO_ERROR_NULL = -886;

constexpr aaudio_direction_t AAUDIO_DIRECTION_OUTPUT = 0;
constexpr aaudio_direction_t AAUDIO_DIRECTION_INPUT = 1;

constexpr aaudio_format_t AAUDIO_FORMAT_INVALID = -1;
constexpr aaudio_format_t AAUDIO_FORMAT_UNSPECIFIED = 0;
constexpr aaudio_format_t AAUDIO_FORMAT_PCM_I16 = 1;
constexpr aaudio_format_t AAUDIO_FORMAT_PCM_FLOAT = 2;
constexpr aaudio_format_t AAUDIO_FORMAT_PCM_I24_PACKED = 3;
constexpr aaudio_format_t AAUDIO_FORMAT_PCM_I32 = 4;

constexpr aaudio_stream_state_t AAUDIO_STREAM_STATE_OPEN = 2;
constexpr aaudio_stream_state_t AAUDIO_STREAM_STATE_STARTED = 4;
constexpr aaudio_stream_state_t AAUDIO_STREAM_STATE_PAUSED = 6;
constexpr aaudio_stream_state_t AAUDIO_STREAM_STATE_STOPPED = 10;
constexpr aaudio_stream_state_t AAUDIO_STREAM_STATE_CLOSED = 12;

constexpr aaudio_data_callback_result_t AAUDIO_CALLBACK_RESULT_CONTINUE = 0;

struct AAudioStreamBuilder;
struct AAudioStream;

using AAudioStream_dataCallback = aaudio_data_callback_result_t (*)(AAudioStream* stream,
                                                                     void* userData,
                                                                     void* audioData,
                                                                     int32_t numFrames);
using AAudioStream_errorCallback = void (*)(AAudioStream* stream, void* userData,
                                             aaudio_result_t error);

// ------------------------------------------------------------ backend choice

/// What `CORDIAL_AUDIO` selected, decided once and logged once.
///
/// The variable did not exist before this file. It was described in
/// conversation as the intended design, then tried on a live run where
/// nothing read it — which looks exactly like a feature that did nothing, and
/// is why the selection is announced at startup rather than left to be
/// inferred from whether sound comes out.
enum class Backend {
    /// FMOD's Java `org.fmod.AudioDevice` path. Was the default until AAudio
    /// had been measured against it; now the fallback, reachable with
    /// `CORDIAL_AUDIO=java`, and still the path a host with no PipeWire takes
    /// because `supportsAAudio()` answers false there whatever this says.
    Java,
    /// AAudio over PipeWire: this file, wired all the way through. **The
    /// default**, since 2026-08-22 — see `docs/analysis/aaudio-contract.md`
    /// for the measurements, and note that they do not show it playing better
    /// than the Java path on this machine. What they show is that it plays the
    /// same and records at all, which the Java path never has.
    AAudio,
    /// AAudio resolvable, every `openStream` honestly refused. **This is the
    /// control**, and it is the reason the switch has three values rather
    /// than two: it answers "does FMOD prefer AAudio once it can see it, and
    /// what does it do when a stream will not open" in the same build as the
    /// working path, so the difference between them is one environment
    /// variable rather than two binaries.
    AAudioRefuse,
};

Backend parse_backend(const char* value) {
    // **AAudio, and the run that was holding it back has happened.**
    //
    // 72dce0a implemented capture and held this at Java because nothing had
    // observed what FMOD does with an input stream that now *succeeds*. The
    // risk was one-directional: if FMOD started recording and never stopped,
    // the microphone indicator would stay lit for the session.
    //
    // Answered on 2026-08-22 by a real signed-in session -- Doors, 1 hour 44
    // minutes, `CORDIAL_AUDIO=aaudio`, the user's own account:
    //
    //     input stream opened   x2      FMOD probing for capabilities
    //     microphone opened      0
    //     microphone closed      0
    //     cordial-audiorecord nodes at the end: 0
    //     AudioDevice.init       0      the Java path never ran at all
    //
    // **FMOD opens input streams and never calls requestStart.** So the
    // microphone is never opened, and the failure this was held for cannot
    // occur on that path. This is the outcome that a two-way "opens are
    // matched by closes" grep would have mistaken for a clean pass, which is
    // why it is written out here: an unasked question is not a passed test,
    // and the reason to flip is that the question was asked and came back
    // zero, not that the log was empty.
    //
    // Still unmeasured: a session with voice chat actually enabled. That is
    // the one path where FMOD would have reason to call requestStart. If a
    // lit microphone indicator is ever reported, that is where to look, and
    // this branch is the one line to change back.
    if (!value || value[0] == '\0') return Backend::AAudio;
    if (std::strcmp(value, "aaudio") == 0) return Backend::AAudio;
    if (std::strcmp(value, "aaudio-refuse") == 0) return Backend::AAudioRefuse;
    if (std::strcmp(value, "java") == 0) return Backend::Java;
    // Falls back to the default rather than to `java`, so that a typo does not
    // quietly select a different backend from the one an untyped run gets.
    std::fprintf(stderr,
        "W/Cordial-AAudio          CORDIAL_AUDIO=%s is not a backend I know "
        "(java, aaudio, aaudio-refuse); using aaudio, the default.\n", value);
    return Backend::AAudio;
}

Backend selected_backend() {
    static const Backend backend = [] {
        Backend b = parse_backend(std::getenv("CORDIAL_AUDIO"));
        const char* name = b == Backend::AAudio          ? "aaudio"
                            : b == Backend::AAudioRefuse ? "aaudio-refuse"
                                                          : "java";
        std::fprintf(stderr,
            "I/Cordial-AAudio          audio backend: %s (CORDIAL_AUDIO=%s). %s\n", name,
            std::getenv("CORDIAL_AUDIO") ? std::getenv("CORDIAL_AUDIO") : "unset",
            b == Backend::Java
                ? "libaaudio.so is not registered and org.fmod.FMOD.supportsAAudio() reports "
                  "false, so FMOD takes its Java AudioDevice path. This is the default, and "
                  "it is held here until one signed-in run shows FMOD closing an input "
                  "stream it opened; CORDIAL_AUDIO=aaudio opts in."
                : b == Backend::AAudio
                      ? "libaaudio.so is registered and supportsAAudio() reports true; streams "
                        "open against PipeWire. Playback is callback-driven, capture is a "
                        "blocking read, and the microphone exists only between requestStart "
                        "and the next stop, pause or close."
                      : "CONTROL RUN: libaaudio.so is registered and supportsAAudio() reports "
                        "true, but every openStream is refused with AAUDIO_ERROR_UNAVAILABLE.");
        return b;
    }();
    return backend;
}

// ------------------------------------------------------------------ objects

struct Builder {
    aaudio_direction_t direction = AAUDIO_DIRECTION_OUTPUT;
    aaudio_format_t format = AAUDIO_FORMAT_UNSPECIFIED;
    int32_t buffer_capacity = 0;
    aaudio_performance_mode_t performance_mode = 0;
    aaudio_usage_t usage = 0;
    aaudio_input_preset_t input_preset = 0;
    AAudioStream_dataCallback data_callback = nullptr;
    void* data_user = nullptr;
    AAudioStream_errorCallback error_callback = nullptr;
    void* error_user = nullptr;
};

/// What an input stream reports, and what `CaptureStream` is asked for.
///
/// **Declared rather than negotiated, and the microphone rule is the reason.**
/// The output path opens a `pw_stream`, waits for PipeWire to pick a format,
/// and reports the measured answer — see `CallbackStream::open`. Capture
/// cannot do that: the engine reads `getSampleRate`/`getChannelCount`/
/// `getFormat` off a stream that has been *opened* and not yet *started*, and
/// no PipeWire capture stream may exist in that state. So the numbers are
/// chosen here and `CaptureStream` is asked for exactly them; PipeWire
/// converts on its side of the link, which it does for capture the same way
/// it does for playback.
///
/// Mono S16 at 48 kHz because that is what the rest of Cordial's capture side
/// already asks for and what voice wants: `CaptureStream` negotiates
/// `SPA_AUDIO_FORMAT_S16` and nothing else, and `WebRtcAudioRecord` in
/// `audio_classes.cpp` is opened mono for the same reason.
constexpr uint32_t kCaptureRate = 48000;
constexpr uint32_t kCaptureChannels = 1;
constexpr uint32_t kCaptureBytesPerFrame = kCaptureChannels * 2;

/// 10 ms, matching `AudioTrack.getLowLatencyInputFramesPerBuffer` in
/// `audio_classes.cpp`. Also declared rather than measured, for the reason
/// above: PipeWire's quantum is only knowable once a stream is connected.
constexpr uint32_t kCaptureBurstFrames = kCaptureRate / 100;

/// Frames `CaptureStream`'s ring holds — half a second, which is the figure
/// `CaptureStream::open` computes for itself in `pipewire_backend.cpp`. Stated
/// twice because the header does not expose it; if that half second ever
/// changes, this is the other place.
constexpr uint32_t kCaptureRingFrames = kCaptureRate / 2;

struct Stream {
    /// OUTPUT drives `pw` below and INPUT drives `capture`; exactly one of
    /// the two is ever opened, and `direction` is how every entry point knows
    /// which set of answers is the truthful one.
    aaudio_direction_t direction = AAUDIO_DIRECTION_OUTPUT;

    /// The host backend, behind ADR-023's seam rather than named here. This
    /// was a `CallbackStream` by value, which is what left no room for a
    /// second backend: every call site below said "PipeWire" by holding one.
    std::unique_ptr<cordial::audio::OutputStream> pw = cordial::audio::make_output_stream();

    /// **Input only, and it holds no PipeWire resource until `requestStart`.**
    ///
    /// This is the whole of the microphone rule as it applies to AAudio.
    /// `AAudioStreamBuilder_openStream` on an input stream allocates this
    /// object and nothing else: an AAudio stream that has been opened and not
    /// started is not recording, so there must be no capture node in the
    /// registry, no lit microphone indicator, and nothing for another
    /// application to see. `requestStart` opens it, `requestPause`,
    /// `requestStop` and `close` all destroy it, and so does this object's
    /// own destructor on every failure path in `openStream`.
    cordial::audio::CaptureStream capture;

    /// A partial frame left over from the previous `AAudioStream_read`.
    ///
    /// `CaptureStream::read` counts bytes, and this bridge owes the engine
    /// whole frames. PipeWire's chunk sizes are frame-aligned and this is the
    /// only reader of that ring, so the remainder should always be zero —
    /// keeping it rather than discarding it means a producer that one day is
    /// not aligned costs a frame of latency instead of shifting every
    /// subsequent sample by one byte.
    uint8_t read_residue[16] = {};
    uint32_t read_residue_len = 0;

    AAudioStream_dataCallback data_callback = nullptr;
    void* data_user = nullptr;
    AAudioStream_errorCallback error_callback = nullptr;
    void* error_user = nullptr;

    aaudio_format_t format = AAUDIO_FORMAT_UNSPECIFIED;
    std::atomic<aaudio_stream_state_t> state{AAUDIO_STREAM_STATE_OPEN};

    /// The thread PipeWire last ran the fill callback on, recorded so that a
    /// `close` reaching this file from inside that callback can be refused
    /// instead of deadlocking on the loop lock. AAudio's own documentation
    /// forbids it ("another thread should be used to stop and close the
    /// stream"), and a well-behaved FMOD will never do it — but a hang is a
    /// far worse way to find out than an error code and a line of log.
    std::atomic<unsigned long> callback_thread{0};

    /// Reported by `AAudioStream_getXRunCount`. See the comment there: this
    /// counts what Cordial can honestly see, which is not what Android counts.
    std::atomic<int32_t> xruns{0};

    // CORDIAL_TRACE_AUDIO=1 bookkeeping only. Touched from the fill callback
    // and nowhere else, so plain members rather than atomics: AAudio's own
    // contract is that the data callback is never entered from two threads at
    // once, and PipeWire honours it here by running `process` on one loop.
    uint64_t trace_cycles = 0;
    uint64_t trace_frames = 0;
    float trace_peak = 0.0f;
    std::chrono::steady_clock::time_point trace_last{};
};

/// Translates one `aaudio_format_t` into the bits/float pair
/// `CallbackStream::open` speaks. Returns false for anything this bridge
/// cannot carry, which the caller reports as a refused open.
bool format_to_bits(aaudio_format_t format, uint32_t& bits, bool& is_float) {
    switch (format) {
    case AAUDIO_FORMAT_UNSPECIFIED:
        // No preference: `CallbackStream::open` reads zero bits as "offer
        // PipeWire's own float", which converts nothing anywhere.
        bits = 0;
        is_float = false;
        return true;
    case AAUDIO_FORMAT_PCM_I16: bits = 16; is_float = false; return true;
    case AAUDIO_FORMAT_PCM_FLOAT: bits = 32; is_float = true; return true;
    case AAUDIO_FORMAT_PCM_I24_PACKED: bits = 24; is_float = false; return true;
    case AAUDIO_FORMAT_PCM_I32: bits = 32; is_float = false; return true;
    default: return false;
    }
}

aaudio_format_t bits_to_format(uint32_t bits, bool is_float) {
    if (is_float) return bits == 32 ? AAUDIO_FORMAT_PCM_FLOAT : AAUDIO_FORMAT_INVALID;
    switch (bits) {
    case 16: return AAUDIO_FORMAT_PCM_I16;
    case 24: return AAUDIO_FORMAT_PCM_I24_PACKED;
    case 32: return AAUDIO_FORMAT_PCM_I32;
    default: return AAUDIO_FORMAT_INVALID;
    }
}

const char* format_name(aaudio_format_t f) {
    switch (f) {
    case AAUDIO_FORMAT_UNSPECIFIED: return "UNSPECIFIED";
    case AAUDIO_FORMAT_PCM_I16: return "PCM_I16";
    case AAUDIO_FORMAT_PCM_FLOAT: return "PCM_FLOAT";
    case AAUDIO_FORMAT_PCM_I24_PACKED: return "PCM_I24_PACKED";
    case AAUDIO_FORMAT_PCM_I32: return "PCM_I32";
    default: return "?";
    }
}

/// `CORDIAL_TRACE_AUDIO=1` only. Off, this is one relaxed bool load per cycle.
bool trace_audio_enabled() {
    static const bool enabled = std::getenv("CORDIAL_TRACE_AUDIO") != nullptr;
    return enabled;
}

/// Largest absolute sample in a freshly filled buffer, as a fraction of full
/// scale.
///
/// This exists because "a game played sound" is otherwise not something this
/// code can honestly claim. A connected PipeWire node proves a stream was
/// opened; `pw-top` proves it is being driven; neither distinguishes sound from a
/// perfectly punctual river of zeroes, which is exactly what a bridge that
/// negotiated the wrong format or handed the engine the wrong frame count
/// would produce. A peak is the cheapest reading that tells those apart.
float buffer_peak(const void* data, uint32_t frames, uint32_t channels, uint32_t bits,
                  bool is_float) {
    const size_t samples = static_cast<size_t>(frames) * channels;
    float peak = 0.0f;
    if (is_float && bits == 32) {
        const auto* p = static_cast<const float*>(data);
        for (size_t i = 0; i < samples; ++i) {
            float v = p[i] < 0.0f ? -p[i] : p[i];
            if (v > peak) peak = v;
        }
    } else if (!is_float && bits == 16) {
        const auto* p = static_cast<const int16_t*>(data);
        for (size_t i = 0; i < samples; ++i) {
            int v = p[i] < 0 ? -static_cast<int>(p[i]) : p[i];
            if (static_cast<float>(v) > peak) peak = static_cast<float>(v);
        }
        peak /= 32768.0f;
    } else if (!is_float && bits == 32) {
        const auto* p = static_cast<const int32_t*>(data);
        for (size_t i = 0; i < samples; ++i) {
            int64_t v = p[i] < 0 ? -static_cast<int64_t>(p[i]) : p[i];
            float f = static_cast<float>(v);
            if (f > peak) peak = f;
        }
        peak /= 2147483648.0f;
    }
    return peak;
}

/// PipeWire's pull, turned into AAudio's push-to-the-app.
///
/// **Realtime.** With the trace off this is two relaxed atomic stores and the
/// engine's own callback: no lock, no allocation, no log. With
/// `CORDIAL_TRACE_AUDIO=1` it also scans the buffer for a peak and prints a
/// line a second, which breaks that rule deliberately and only when asked —
/// the same bargain `PlaybackStream::process` already strikes with the same
/// variable, and the reason it is a switch rather than a default.
bool fill_from_engine(void* dst, uint32_t frames, void* user) {
    auto* s = static_cast<Stream*>(user);
    s->callback_thread.store(cordial_pthread_id(pthread_self()),
                             std::memory_order_relaxed);
    auto cb = s->data_callback;
    if (!cb) return false;
    const bool cont = cb(reinterpret_cast<AAudioStream*>(s), s->data_user, dst,
                          static_cast<int32_t>(frames)) == AAUDIO_CALLBACK_RESULT_CONTINUE;

    if (trace_audio_enabled()) {
        ++s->trace_cycles;
        s->trace_frames += frames;
        float peak = buffer_peak(dst, frames, s->pw->channels(), s->pw->sample_bits(),
                                  s->pw->sample_is_float());
        if (peak > s->trace_peak) s->trace_peak = peak;
        auto now = std::chrono::steady_clock::now();
        if (now - s->trace_last >= std::chrono::seconds(1)) {
            s->trace_last = now;
            std::fprintf(stderr,
                "D/Cordial-AAudio          audio trace: %llu callback(s), %llu frame(s), peak "
                "%.4f of full scale since the last line\n",
                static_cast<unsigned long long>(s->trace_cycles),
                static_cast<unsigned long long>(s->trace_frames), s->trace_peak);
            s->trace_peak = 0.0f;
        }
    }
    return cont;
}

/// The same reading as `buffer_peak`, for samples coming the other way.
///
/// The playback bridge earned its "audio actually happened" claim with a peak
/// meter in the fill callback, because a connected node and a punctual cycle
/// count are both equally happy with a river of zeroes. Capture has exactly
/// the same hole and it is worse: a microphone that is muted at the source, or
/// a stream linked to the wrong node, delivers frames forever and every one of
/// them is zero. Runs on the engine's own reading thread, never on PipeWire's.
float s16_peak(const int16_t* p, size_t samples) {
    int peak = 0;
    for (size_t i = 0; i < samples; ++i) {
        int v = p[i] < 0 ? -static_cast<int>(p[i]) : p[i];
        if (v > peak) peak = v;
    }
    return static_cast<float>(peak) / 32768.0f;
}

/// Sleeps without touching `std::this_thread`, which would pull `<thread>` in
/// for one call. `AAudioStream_read`'s only way to wait: `CaptureStream` hands
/// out whatever has arrived and never blocks, deliberately, and giving it a
/// condition variable would mean signalling from inside its `process()` — the
/// one path in this backend that is not allowed to grow a wakeup.
void sleep_ns(int64_t ns) {
    if (ns <= 0) return;
    timespec ts{};
    ts.tv_sec = static_cast<time_t>(ns / 1000000000);
    ts.tv_nsec = static_cast<long>(ns % 1000000000);
    nanosleep(&ts, nullptr);
}

bool on_callback_thread(Stream* s) {
    unsigned long t = s->callback_thread.load(std::memory_order_relaxed);
    return t != 0 && t == cordial_pthread_id(pthread_self());
}

/// `AAudioStreamBuilder_openStream` for `AAUDIO_DIRECTION_INPUT`.
///
/// **Opening an input stream opens nothing.** That sentence is the whole
/// design. The object this returns holds a `CaptureStream` that holds no
/// `pw_stream`, so between here and `AAudioStream_requestStart` there is no
/// capture node in PipeWire's registry, no microphone indicator, and nothing
/// for `pw-cli ls Node` to find — which is what the rule at the top of
/// `audio_classes.cpp` requires and what a run of `audio_probe aaudio-record`
/// checks from outside.
///
/// It also means the getters cannot report a negotiated format the way the
/// output path does, since negotiating one would mean connecting a stream.
/// They report `kCapture*` above instead, and `CaptureStream` is asked for
/// exactly those, so what the engine is told and what it will be handed are
/// the same numbers rather than two guesses.
aaudio_result_t open_input_stream(const Builder& b, AAudioStream** streamOut) {
    // S16 is all `CaptureStream` negotiates, so it is all this can honestly
    // report. UNSPECIFIED is what the 2026-08-22 run measured FMOD asking for
    // and means "you choose"; an explicit request for anything else is
    // refused rather than quietly answered with I16, because the engine lays
    // its recording buffers out against `getFormat` and a wrong answer there
    // is noise rather than an error.
    if (b.format != AAUDIO_FORMAT_UNSPECIFIED && b.format != AAUDIO_FORMAT_PCM_I16) {
        std::fprintf(stderr,
            "E/Cordial-AAudio          refusing an input stream asking for %s: the PipeWire "
            "capture path negotiates PCM_I16 and nothing else. Refusing costs recording; "
            "answering with I16 anyway would cost it audibly and silently.\n",
            format_name(b.format));
        return AAUDIO_ERROR_ILLEGAL_ARGUMENT;
    }

    // A callback-driven *input* stream is a shape AAudio has and this bridge
    // does not: it would need a thread of our own pulling `CaptureStream` and
    // pushing into the engine, with the microphone's lifetime tied to that
    // thread rather than to `requestStart`. The 2026-08-22 measurement has
    // FMOD opening its input stream with `dataCallback=no`, so this is the
    // branch that should never be taken — and if a later build takes it, an
    // error here is a line in the log, where opening the stream and never
    // invoking the callback would be a microphone held open for a recording
    // nobody receives. That is the exact state the microphone rule exists to
    // forbid, so it is not a close call.
    if (b.data_callback) {
        std::fprintf(stderr,
            "E/Cordial-AAudio          refusing an input stream that installed a data "
            "callback: this bridge implements capture as the blocking AAudioStream_read the "
            "engine's own symbol set implies (there is no AAudioStream_write), and has no "
            "thread to push frames from. Recording will not work this run; say so rather "
            "than hold the microphone open for a callback that is never called.\n");
        return AAUDIO_ERROR_UNIMPLEMENTED;
    }

    auto* s = new (std::nothrow) Stream();
    if (!s) return AAUDIO_ERROR_NO_MEMORY;
    s->direction = AAUDIO_DIRECTION_INPUT;
    s->error_callback = b.error_callback;
    s->error_user = b.error_user;
    s->format = AAUDIO_FORMAT_PCM_I16;
    s->state.store(AAUDIO_STREAM_STATE_OPEN, std::memory_order_relaxed);

    std::fprintf(stderr,
        "I/Cordial-AAudio          input stream opened: will report %u Hz, %u channel(s), "
        "%s, %u frames per burst. **No microphone is open yet** — the capture stream is "
        "created by requestStart and destroyed by requestPause, requestStop and close, so "
        "an opened-but-unstarted stream is invisible to pw-cli and leaves the desktop's "
        "microphone indicator out.\n",
        kCaptureRate, kCaptureChannels, format_name(s->format), kCaptureBurstFrames);

    *streamOut = reinterpret_cast<AAudioStream*>(s);
    return AAUDIO_OK;
}

} // namespace

// --------------------------------------------------------------- entry points
//
// Exported through `cordial_aaudio_symbols` below rather than as real ELF
// exports: nothing on the host links `libaaudio.so`, the guest reaches it
// through the bionic linker's virtual-library table, and keeping these
// internal means the host's own dynamic symbol table gains no `AAudio*` names
// that could shadow anything.

extern "C" {

static aaudio_result_t AAudio_createStreamBuilder(AAudioStreamBuilder** builder) {
    if (!builder) return AAUDIO_ERROR_NULL;
    auto* b = new (std::nothrow) Builder();
    if (!b) return AAUDIO_ERROR_NO_MEMORY;
    *builder = reinterpret_cast<AAudioStreamBuilder*>(b);
    return AAUDIO_OK;
}

static aaudio_result_t AAudioStreamBuilder_delete(AAudioStreamBuilder* builder) {
    if (!builder) return AAUDIO_ERROR_NULL;
    delete reinterpret_cast<Builder*>(builder);
    return AAUDIO_OK;
}

static void AAudioStreamBuilder_setDirection(AAudioStreamBuilder* builder,
                                              aaudio_direction_t direction) {
    if (builder) reinterpret_cast<Builder*>(builder)->direction = direction;
}

static void AAudioStreamBuilder_setFormat(AAudioStreamBuilder* builder, aaudio_format_t format) {
    if (builder) reinterpret_cast<Builder*>(builder)->format = format;
}

static void AAudioStreamBuilder_setBufferCapacityInFrames(AAudioStreamBuilder* builder,
                                                           int32_t numFrames) {
    if (builder) reinterpret_cast<Builder*>(builder)->buffer_capacity = numFrames;
}

static void AAudioStreamBuilder_setPerformanceMode(AAudioStreamBuilder* builder,
                                                    aaudio_performance_mode_t mode) {
    if (builder) reinterpret_cast<Builder*>(builder)->performance_mode = mode;
}

static void AAudioStreamBuilder_setUsage(AAudioStreamBuilder* builder, aaudio_usage_t usage) {
    if (builder) reinterpret_cast<Builder*>(builder)->usage = usage;
}

static void AAudioStreamBuilder_setInputPreset(AAudioStreamBuilder* builder,
                                                aaudio_input_preset_t preset) {
    if (builder) reinterpret_cast<Builder*>(builder)->input_preset = preset;
}

static void AAudioStreamBuilder_setDataCallback(AAudioStreamBuilder* builder,
                                                 AAudioStream_dataCallback callback,
                                                 void* userData) {
    if (!builder) return;
    auto* b = reinterpret_cast<Builder*>(builder);
    b->data_callback = callback;
    b->data_user = userData;
}

static void AAudioStreamBuilder_setErrorCallback(AAudioStreamBuilder* builder,
                                                  AAudioStream_errorCallback callback,
                                                  void* userData) {
    if (!builder) return;
    auto* b = reinterpret_cast<Builder*>(builder);
    b->error_callback = callback;
    b->error_user = userData;
}

/// The one call that can honestly fail, and the one worth narrating.
///
/// Everything the engine configured is logged here whatever the outcome,
/// because this is the only place the *requested* configuration is visible —
/// the getters below report the negotiated one, and the difference between
/// them is the whole answer to "what does this build actually want".
static aaudio_result_t AAudioStreamBuilder_openStream(AAudioStreamBuilder* builder,
                                                       AAudioStream** streamOut) {
    if (!builder || !streamOut) return AAUDIO_ERROR_NULL;
    auto* b = reinterpret_cast<Builder*>(builder);
    *streamOut = nullptr;

    std::fprintf(stderr,
        "I/Cordial-AAudio          openStream requested: direction=%s format=%s "
        "bufferCapacity=%d performanceMode=%d usage=%d inputPreset=%d dataCallback=%s "
        "errorCallback=%s\n",
        b->direction == AAUDIO_DIRECTION_INPUT ? "INPUT" : "OUTPUT", format_name(b->format),
        b->buffer_capacity, b->performance_mode, b->usage, b->input_preset,
        b->data_callback ? "yes" : "no", b->error_callback ? "yes" : "no");

    if (selected_backend() == Backend::AAudioRefuse) {
        std::fprintf(stderr,
            "I/Cordial-AAudio          CONTROL RUN (CORDIAL_AUDIO=aaudio-refuse): reporting "
            "AAUDIO_ERROR_UNAVAILABLE so that what FMOD does next can be read off the log.\n");
        return AAUDIO_ERROR_UNAVAILABLE;
    }

    uint32_t bits = 0;
    bool is_float = false;
    if (!format_to_bits(b->format, bits, is_float)) {
        std::fprintf(stderr,
            "E/Cordial-AAudio          no PipeWire equivalent for aaudio_format_t %d; "
            "refusing rather than substituting a nearby format.\n", b->format);
        return AAUDIO_ERROR_ILLEGAL_ARGUMENT;
    }

    if (!cordial::audio::host_backend_available()) {
        // Should be unreachable: `org.fmod.FMOD.supportsAAudio()` in
        // `audio_classes.cpp` asks the same question first and answers false
        // when there is no session, precisely so that FMOD never gets here.
        // Kept because a refusal at this point is not recoverable — the
        // control run on 2026-08-22 showed FMOD abandoning audio outright
        // rather than falling back — so the one place that could still
        // produce it should say what it cost.
        std::fprintf(stderr,
            "E/Cordial-AAudio          no PipeWire session reachable, and FMOD has already "
            "committed to AAudio; there will be no audio this run and no fallback. This "
            "means supportsAAudio() said yes and the session went away between then and "
            "now.\n");
        return AAUDIO_ERROR_UNAVAILABLE;
    }

    if (b->direction == AAUDIO_DIRECTION_INPUT) return open_input_stream(*b, streamOut);

    // A stream with no data callback is not a mistake and must not be
    // refused.
    //
    // **Measured**, on a signed-in run into place 1818 on 2026-08-22. FMOD
    // opens four output streams in this order, and only the last is the one
    // that plays:
    //
    //     bufferCapacity=0     dataCallback=no   errorCallback=no    -> closed at once
    //     bufferCapacity=0     dataCallback=yes  errorCallback=yes   -> closed at once
    //     bufferCapacity=9216  dataCallback=yes  errorCallback=yes   -> kept, and pulled
    //     direction=INPUT      dataCallback=no   errorCallback=no    -> refused, below
    //
    // The first two are *probes*. With no `AAudioStreamBuilder_setSampleRate`
    // to ask with, the only way to learn what the device runs at is to open a
    // stream and read `getSampleRate`/`getFramesPerBurst` off it — and the
    // 9216 in the third line is FMOD sizing itself from what the second one
    // reported (nine times the 1024-frame burst PipeWire gave it).
    //
    // The first revision of this file refused the callback-less probe, on the
    // reasoning that such a stream could never be fed. True, and beside the
    // point: nothing is meant to feed it. So it opens here like any other, and
    // `CallbackStream` simply never has anything to pull — `fill_from_engine`
    // returns false on a null callback and every cycle is silence.
    auto* s = new (std::nothrow) Stream();
    if (!s) return AAUDIO_ERROR_NO_MEMORY;
    s->data_callback = b->data_callback;
    s->data_user = b->data_user;
    s->error_callback = b->error_callback;
    s->error_user = b->error_user;

    // The user's sink, or empty for the session default. Not something the
    // engine can express: `AAudioStreamBuilder_setDeviceId` is absent from the
    // 25 symbols this build dlsyms, so nothing on this path will ever be asked
    // for a particular output and the choice has to arrive from outside the
    // engine entirely. See `docs/analysis/aaudio-contract.md`.
    if (!s->pw->open(bits, is_float, "Cordial (Roblox via AAudio)",
                    cordial::audio::configured_output_device().c_str(), &fill_from_engine, s)) {
        delete s;
        return AAUDIO_ERROR_UNAVAILABLE;
    }

    s->format = bits_to_format(s->pw->sample_bits(), s->pw->sample_is_float());
    if (s->format == AAUDIO_FORMAT_INVALID) {
        std::fprintf(stderr,
            "E/Cordial-AAudio          PipeWire negotiated a %u-bit %s format with no "
            "aaudio_format_t to describe it; refusing rather than reporting a format the "
            "engine would lay its mixer out against wrongly.\n",
            s->pw->sample_bits(), s->pw->sample_is_float() ? "float" : "integer");
        delete s;
        return AAUDIO_ERROR_UNAVAILABLE;
    }

    std::fprintf(stderr,
        "I/Cordial-AAudio          openStream ok: PipeWire negotiated %u Hz, %u channel(s), "
        "%s, %u frames per burst. Requested format was %s; rate and channel count were left "
        "for PipeWire to choose, which is why nothing resamples.\n",
        s->pw->rate_hz(), s->pw->channels(), format_name(s->format), s->pw->burst_frames(),
        format_name(b->format));

    *streamOut = reinterpret_cast<AAudioStream*>(s);
    return AAUDIO_OK;
}

static aaudio_result_t AAudioStream_close(AAudioStream* stream) {
    if (!stream) return AAUDIO_ERROR_NULL;
    auto* s = reinterpret_cast<Stream*>(stream);
    if (s->direction == AAUDIO_DIRECTION_INPUT) {
        // Unconditional, before anything else can fail. `CaptureStream::close`
        // is idempotent and destroys the `pw_stream` rather than deactivating
        // it, so a closed AAudio input stream is indistinguishable from one
        // that never recorded. `~Stream` would do this too; doing it here as
        // well means no future early return between this line and the delete
        // can leave the microphone up.
        s->capture.close();
        s->state.store(AAUDIO_STREAM_STATE_CLOSED, std::memory_order_relaxed);
        std::fprintf(stderr,
            "I/Cordial-AAudio          input stream closed; %u capture stream(s) still "
            "open across Cordial (must be 0 unless something else is recording).\n",
            cordial::audio::active_capture_streams());
        delete s;
        return AAUDIO_OK;
    }
    if (on_callback_thread(s)) {
        // Would take PipeWire's loop lock from the loop thread itself, which
        // is a plain mutex and would hang the graph. AAudio documents that
        // this is not allowed; refusing keeps the bug visible.
        std::fprintf(stderr,
            "E/Cordial-AAudio          AAudioStream_close called from inside the data "
            "callback; refusing (AAudio requires another thread for this) rather than "
            "deadlocking PipeWire's loop.\n");
        return AAUDIO_ERROR_INVALID_STATE;
    }
    s->pw->close();
    s->state.store(AAUDIO_STREAM_STATE_CLOSED, std::memory_order_relaxed);
    std::fprintf(stderr,
        "I/Cordial-AAudio          stream closed after %llu silence-filled cycle(s).\n",
        static_cast<unsigned long long>(s->pw->silence_cycles()));
    delete s;
    return AAUDIO_OK;
}

// requestStart/Pause/Stop flip one relaxed atomic and touch nothing else.
//
// They deliberately do *not* call `pw_stream_set_active`, which would need
// PipeWire's loop lock — and AAudio documents that an error callback may stop
// the stream, which arrives on the loop thread, where taking that lock hangs.
// The cost is a PipeWire node that stays connected and writes silence while
// paused; the benefit is that the state machine cannot deadlock from anywhere
// it might be called, which is the entire point of moving off the JNI path.
static aaudio_result_t AAudioStream_requestStart(AAudioStream* stream) {
    if (!stream) return AAUDIO_ERROR_NULL;
    auto* s = reinterpret_cast<Stream*>(stream);

    if (s->direction == AAUDIO_DIRECTION_INPUT) {
        // **The only place in this file that opens a microphone**, and the
        // only place that may be. `requestStart` is AAudio's "I am recording
        // now"; everything earlier is preparation and must leave the capture
        // device alone.
        if (s->capture.is_open()) {
            s->state.store(AAUDIO_STREAM_STATE_STARTED, std::memory_order_relaxed);
            return AAUDIO_OK;
        }
        // Empty target: follow whatever PipeWire calls the default source,
        // and keep following it. The same argument `configured_output_device`
        // makes for sinks — a resolved name is a snapshot of today's default,
        // an absent `PW_KEY_TARGET_OBJECT` is a standing instruction — and it
        // also spares the recording path a registry round trip.
        if (!s->capture.open(kCaptureRate, kCaptureChannels, std::string())) {
            // Stays STOPPED with nothing open. A stream that reported OK here
            // would have the engine reading zeroes out of a device it does
            // not hold, which is the "stub that lies" AGENTS.md is about.
            s->capture.close();
            s->state.store(AAUDIO_STREAM_STATE_STOPPED, std::memory_order_relaxed);
            std::fprintf(stderr,
                "E/Cordial-AAudio          requestStart could not open a capture stream; "
                "staying stopped with no microphone open rather than reporting a recording "
                "that is not happening.\n");
            return AAUDIO_ERROR_UNAVAILABLE;
        }
        s->read_residue_len = 0;
        s->trace_cycles = 0;
        s->trace_frames = 0;
        s->trace_peak = 0.0f;
        s->trace_last = std::chrono::steady_clock::now();
        s->state.store(AAUDIO_STREAM_STATE_STARTED, std::memory_order_relaxed);
        return AAUDIO_OK;
    }

    if (!s->pw->is_open()) return AAUDIO_ERROR_INVALID_STATE;
    s->pw->set_running(true);
    s->state.store(AAUDIO_STREAM_STATE_STARTED, std::memory_order_relaxed);
    return AAUDIO_OK;
}

/// Stopping and pausing are the same act on the capture side, and the
/// difference between them is exactly what the microphone rule refuses to
/// honour.
///
/// AAudio's pause is "stop consuming, keep the stream". For output that is a
/// flag on `CallbackStream` and the node stays connected writing silence,
/// which costs nothing anybody can see. For input the equivalent would be a
/// `pw_stream` left in the graph with the samples thrown away — the desktop's
/// microphone indicator still lit, every other application still seeing
/// Cordial holding the capture device, and no way for a user to tell it from
/// recording. So an input pause destroys the stream, the same as a stop, and
/// a later `requestStart` opens a fresh one.
static void stop_capture(Stream* s, aaudio_stream_state_t to) {
    const bool was_open = s->capture.is_open();
    s->capture.close();
    s->read_residue_len = 0;
    s->state.store(to, std::memory_order_relaxed);
    if (was_open) {
        std::fprintf(stderr,
            "I/Cordial-AAudio          recording %s: capture stream destroyed, not "
            "deactivated. %u capture stream(s) still open across Cordial.\n",
            to == AAUDIO_STREAM_STATE_PAUSED ? "paused" : "stopped",
            cordial::audio::active_capture_streams());
    }
}

static aaudio_result_t AAudioStream_requestPause(AAudioStream* stream) {
    if (!stream) return AAUDIO_ERROR_NULL;
    auto* s = reinterpret_cast<Stream*>(stream);
    if (s->direction == AAUDIO_DIRECTION_INPUT) {
        stop_capture(s, AAUDIO_STREAM_STATE_PAUSED);
        return AAUDIO_OK;
    }
    s->pw->set_running(false);
    s->state.store(AAUDIO_STREAM_STATE_PAUSED, std::memory_order_relaxed);
    return AAUDIO_OK;
}

static aaudio_result_t AAudioStream_requestStop(AAudioStream* stream) {
    if (!stream) return AAUDIO_ERROR_NULL;
    auto* s = reinterpret_cast<Stream*>(stream);
    if (s->direction == AAUDIO_DIRECTION_INPUT) {
        stop_capture(s, AAUDIO_STREAM_STATE_STOPPED);
        return AAUDIO_OK;
    }
    s->pw->set_running(false);
    s->state.store(AAUDIO_STREAM_STATE_STOPPED, std::memory_order_relaxed);
    return AAUDIO_OK;
}

static aaudio_stream_state_t AAudioStream_getState(AAudioStream* stream) {
    if (!stream) return AAUDIO_STREAM_STATE_CLOSED;
    return reinterpret_cast<Stream*>(stream)->state.load(std::memory_order_relaxed);
}

static int32_t AAudioStream_getSampleRate(AAudioStream* stream) {
    if (!stream) return 0;
    auto* s = reinterpret_cast<Stream*>(stream);
    if (s->direction == AAUDIO_DIRECTION_INPUT) return static_cast<int32_t>(kCaptureRate);
    return static_cast<int32_t>(s->pw->rate_hz());
}

static int32_t AAudioStream_getChannelCount(AAudioStream* stream) {
    if (!stream) return 0;
    auto* s = reinterpret_cast<Stream*>(stream);
    if (s->direction == AAUDIO_DIRECTION_INPUT) return static_cast<int32_t>(kCaptureChannels);
    return static_cast<int32_t>(s->pw->channels());
}

static aaudio_format_t AAudioStream_getFormat(AAudioStream* stream) {
    if (!stream) return AAUDIO_FORMAT_INVALID;
    return reinterpret_cast<Stream*>(stream)->format;
}

/// The graph quantum PipeWire last asked for, measured rather than declared —
/// on output. On input it is `kCaptureBurstFrames`, which is declared, for
/// the reason given there: measuring it would mean connecting a stream, and
/// the engine reads this off a stream that has not started recording.
static int32_t AAudioStream_getFramesPerBurst(AAudioStream* stream) {
    if (!stream) return 0;
    auto* s = reinterpret_cast<Stream*>(stream);
    if (s->direction == AAUDIO_DIRECTION_INPUT) return static_cast<int32_t>(kCaptureBurstFrames);
    return static_cast<int32_t>(s->pw->burst_frames());
}

// On output, capacity and size are both the burst, and that is the truth
// rather than a shortcut: there is no buffer between the engine and PipeWire
// on that path. PipeWire pulls one quantum and the engine fills it in the same
// call, so there is nothing that could hold more, and reporting a larger
// number would invite FMOD to believe it had latency headroom it does not
// have.
//
// Input is genuinely different and reports so. `CaptureStream` owns a
// half-second ring precisely because its reader turns up on its own schedule,
// so there *is* a buffer, and its size is the honest answer to how far behind
// a reader may fall before it loses samples.
static int32_t AAudioStream_getBufferCapacityInFrames(AAudioStream* stream) {
    if (!stream) return 0;
    auto* s = reinterpret_cast<Stream*>(stream);
    if (s->direction == AAUDIO_DIRECTION_INPUT) return static_cast<int32_t>(kCaptureRingFrames);
    return AAudioStream_getFramesPerBurst(stream);
}

static int32_t AAudioStream_getBufferSizeInFrames(AAudioStream* stream) {
    return AAudioStream_getBufferCapacityInFrames(stream);
}

/// Returns the size actually in force, which AAudio documents as the correct
/// answer when the request is clamped. Here it is always the burst, for the
/// reason above.
static aaudio_result_t AAudioStream_setBufferSizeInFrames(AAudioStream* stream,
                                                           int32_t numFrames) {
    if (!stream) return AAUDIO_ERROR_NULL;
    int32_t actual = AAudioStream_getBufferCapacityInFrames(stream);
    if (numFrames != actual) {
        std::fprintf(stderr,
            "I/Cordial-AAudio          setBufferSizeInFrames(%d) -> %d: on output the buffer "
            "is PipeWire's quantum with nothing between it and the engine to resize, and on "
            "input it is CaptureStream's ring, which is sized in one place.\n",
            numFrames, actual);
    }
    return actual;
}

/// **This is not Android's xrun count, and saying so is the point.**
///
/// On Android this is AudioFlinger's tally of times the client missed a
/// deadline. Here the engine's callback is invoked *synchronously* from
/// PipeWire's own cycle, so a late callback is a late PipeWire cycle: the
/// glitch is real and audible, but it is counted on the server side, and
/// nothing in `pw_time` or the stream API hands it back to the client. What
/// this reports is the only underrun Cordial can see for itself — cycles that
/// had to be filled with silence while the stream was running, which in a
/// healthy run is zero and stays zero.
///
/// So a flat zero here is **not** evidence of a clean run. The instrument for
/// that is `pw-top`'s ERR column against Cordial's node, which counts the
/// server's own xruns and is comparable across this backend and the Java one.
/// This project has four "fixes" on record that were scored with an
/// instrument that turned out to be constant across every run; this comment
/// is here so that this counter does not become the fifth.
///
/// **Input is the exception, and there the number means something.** The ring
/// a capture stream fills is Cordial's own, so a reader that falls half a
/// second behind loses samples where this file can count them:
/// `CaptureStream::dropped_bytes` is a real overrun tally and this reports it
/// in frames. A non-zero reading there is evidence; a zero one still is not,
/// because it says nothing about the server side.
static int32_t AAudioStream_getXRunCount(AAudioStream* stream) {
    if (!stream) return 0;
    auto* s = reinterpret_cast<Stream*>(stream);
    uint64_t count = s->direction == AAUDIO_DIRECTION_INPUT
                          ? s->capture.dropped_bytes() / kCaptureBytesPerFrame
                          : s->pw->silence_cycles();
    return static_cast<int32_t>(count > INT32_MAX ? INT32_MAX : count);
}

/// The capture side, and the only way frames leave this bridge.
///
/// `AAudioStream_write` is not among the 25 names this build looks up and
/// `_read` is, which is the measurement the whole shape rests on: playback is
/// pushed to the engine by a callback, capture is pulled by the engine on a
/// thread of its own. So there is nothing realtime here — this runs wherever
/// FMOD's recording loop runs, and it is allowed to sleep, which is what makes
/// a blocking read implementable over a ring that never blocks.
///
/// The wait is a poll rather than a condition variable, and that is a
/// deliberate trade. Signalling a condvar would mean adding a wakeup to
/// `CaptureStream::Impl::process`, which is PipeWire's realtime thread; a
/// millisecond of latency on this side is much the cheaper of the two.
///
/// AAudio's contract, kept exactly: a non-positive timeout is a non-blocking
/// read that returns what is there; a positive one waits up to that long for
/// `numFrames` and returns however many it got. A short read is not an error.
static aaudio_result_t AAudioStream_read(AAudioStream* stream, void* buffer, int32_t numFrames,
                                          int64_t timeoutNanoseconds) {
    if (!stream || !buffer) return AAUDIO_ERROR_NULL;
    auto* s = reinterpret_cast<Stream*>(stream);

    if (s->direction != AAUDIO_DIRECTION_INPUT) {
        static bool warned = false;
        if (!warned) {
            warned = true;
            std::fprintf(stderr,
                "E/Cordial-AAudio          AAudioStream_read on an output stream; reporting "
                "AAUDIO_ERROR_UNIMPLEMENTED.\n");
        }
        return AAUDIO_ERROR_UNIMPLEMENTED;
    }

    if (numFrames < 0) return AAUDIO_ERROR_ILLEGAL_ARGUMENT;
    if (numFrames == 0) return 0;
    // Not started, or started and then stopped: there is no microphone open,
    // and the engine must hear that rather than be handed a quiet room.
    if (s->state.load(std::memory_order_relaxed) != AAUDIO_STREAM_STATE_STARTED ||
        !s->capture.is_open()) {
        return AAUDIO_ERROR_INVALID_STATE;
    }

    const uint32_t bpf = kCaptureBytesPerFrame;
    if (static_cast<uint32_t>(numFrames) > UINT32_MAX / bpf) return AAUDIO_ERROR_ILLEGAL_ARGUMENT;
    const uint32_t want = static_cast<uint32_t>(numFrames) * bpf;
    auto* dst = static_cast<uint8_t*>(buffer);
    uint32_t got = 0;

    // The partial frame the previous call could not report. Always shorter
    // than one frame and `want` is at least one frame, so this never fills
    // the buffer on its own and never has to be carried twice.
    if (s->read_residue_len > 0) {
        std::memcpy(dst, s->read_residue, s->read_residue_len);
        got = s->read_residue_len;
        s->read_residue_len = 0;
    }

    const auto deadline =
        std::chrono::steady_clock::now() +
        std::chrono::nanoseconds(timeoutNanoseconds > 0 ? timeoutNanoseconds : 0);
    for (;;) {
        got += s->capture.read(dst + got, want - got);
        if (got >= want || timeoutNanoseconds <= 0) break;
        if (std::chrono::steady_clock::now() >= deadline) break;
        // A stop from another thread must end the wait rather than let it run
        // out the clock; `CaptureStream::close` is safe against a concurrent
        // `read` (the ring outlives the `pw_stream`), but there will be no
        // more samples, so waiting for them is waiting for nothing.
        if (s->state.load(std::memory_order_relaxed) != AAUDIO_STREAM_STATE_STARTED) break;
        // A millisecond, or whatever is left of the timeout if that is less.
        auto remaining = std::chrono::duration_cast<std::chrono::nanoseconds>(
                              deadline - std::chrono::steady_clock::now())
                              .count();
        sleep_ns(remaining < 1000000 ? remaining : 1000000);
    }

    const uint32_t frames = got / bpf;
    const uint32_t remainder = got % bpf;
    if (remainder > 0 && remainder <= sizeof s->read_residue) {
        std::memcpy(s->read_residue, dst + frames * bpf, remainder);
        s->read_residue_len = remainder;
    }

    if (trace_audio_enabled() && frames > 0) {
        ++s->trace_cycles;
        s->trace_frames += frames;
        float peak = s16_peak(reinterpret_cast<const int16_t*>(dst),
                               static_cast<size_t>(frames) * kCaptureChannels);
        if (peak > s->trace_peak) s->trace_peak = peak;
        auto now = std::chrono::steady_clock::now();
        if (now - s->trace_last >= std::chrono::seconds(1)) {
            s->trace_last = now;
            std::fprintf(stderr,
                "D/Cordial-AAudio          capture trace: %llu read(s), %llu frame(s), peak "
                "%.4f of full scale, %llu frame(s) dropped by the ring in total\n",
                static_cast<unsigned long long>(s->trace_cycles),
                static_cast<unsigned long long>(s->trace_frames), s->trace_peak,
                static_cast<unsigned long long>(s->capture.dropped_bytes() / bpf));
            s->trace_peak = 0.0f;
        }
    }

    return static_cast<aaudio_result_t>(frames);
}

// ------------------------------------------------------------- symbol table

struct CordialAAudioSymbol {
    const char* name;
    void* address;
};

static const CordialAAudioSymbol kSymbols[] = {
    {"AAudio_createStreamBuilder", reinterpret_cast<void*>(&AAudio_createStreamBuilder)},
    {"AAudioStreamBuilder_delete", reinterpret_cast<void*>(&AAudioStreamBuilder_delete)},
    {"AAudioStreamBuilder_openStream", reinterpret_cast<void*>(&AAudioStreamBuilder_openStream)},
    {"AAudioStreamBuilder_setBufferCapacityInFrames",
     reinterpret_cast<void*>(&AAudioStreamBuilder_setBufferCapacityInFrames)},
    {"AAudioStreamBuilder_setDataCallback",
     reinterpret_cast<void*>(&AAudioStreamBuilder_setDataCallback)},
    {"AAudioStreamBuilder_setDirection", reinterpret_cast<void*>(&AAudioStreamBuilder_setDirection)},
    {"AAudioStreamBuilder_setErrorCallback",
     reinterpret_cast<void*>(&AAudioStreamBuilder_setErrorCallback)},
    {"AAudioStreamBuilder_setFormat", reinterpret_cast<void*>(&AAudioStreamBuilder_setFormat)},
    {"AAudioStreamBuilder_setInputPreset",
     reinterpret_cast<void*>(&AAudioStreamBuilder_setInputPreset)},
    {"AAudioStreamBuilder_setPerformanceMode",
     reinterpret_cast<void*>(&AAudioStreamBuilder_setPerformanceMode)},
    {"AAudioStreamBuilder_setUsage", reinterpret_cast<void*>(&AAudioStreamBuilder_setUsage)},
    {"AAudioStream_close", reinterpret_cast<void*>(&AAudioStream_close)},
    {"AAudioStream_getBufferCapacityInFrames",
     reinterpret_cast<void*>(&AAudioStream_getBufferCapacityInFrames)},
    {"AAudioStream_getBufferSizeInFrames",
     reinterpret_cast<void*>(&AAudioStream_getBufferSizeInFrames)},
    {"AAudioStream_getChannelCount", reinterpret_cast<void*>(&AAudioStream_getChannelCount)},
    {"AAudioStream_getFormat", reinterpret_cast<void*>(&AAudioStream_getFormat)},
    {"AAudioStream_getFramesPerBurst", reinterpret_cast<void*>(&AAudioStream_getFramesPerBurst)},
    {"AAudioStream_getSampleRate", reinterpret_cast<void*>(&AAudioStream_getSampleRate)},
    {"AAudioStream_getState", reinterpret_cast<void*>(&AAudioStream_getState)},
    {"AAudioStream_getXRunCount", reinterpret_cast<void*>(&AAudioStream_getXRunCount)},
    {"AAudioStream_read", reinterpret_cast<void*>(&AAudioStream_read)},
    {"AAudioStream_requestPause", reinterpret_cast<void*>(&AAudioStream_requestPause)},
    {"AAudioStream_requestStart", reinterpret_cast<void*>(&AAudioStream_requestStart)},
    {"AAudioStream_requestStop", reinterpret_cast<void*>(&AAudioStream_requestStop)},
    {"AAudioStream_setBufferSizeInFrames",
     reinterpret_cast<void*>(&AAudioStream_setBufferSizeInFrames)},
};

const CordialAAudioSymbol* cordial_aaudio_symbols(size_t* count) {
    if (count) *count = sizeof(kSymbols) / sizeof(kSymbols[0]);
    return kSymbols;
}

int cordial_audio_backend_is_aaudio(void) {
    return selected_backend() == Backend::Java ? 0 : 1;
}

void cordial_audio_backend_announce(void) {
    (void)selected_backend();
    // **Said out loud, because `pipewire_backend.h` has claimed for some time
    // that it was and it never was.** A user on 2026-08-27 set
    // `CORDIAL_AUDIO_HOST=oss`, got "no audio", and had no way to tell whether
    // their variable had been seen -- it had not, and one line here would have
    // said so in ten seconds rather than costing a diagnosis.
    //
    // Both names, because they answer different questions: what was asked for,
    // and what is actually there. When nobody asked, the first reads `auto` and
    // the second is the probe's answer.
    const char* asked = cordial::audio::host_backend_name();
    const char* got = cordial::audio::effective_backend_name();
    if (std::strcmp(asked, got) == 0) {
        std::fprintf(stderr, "I/Cordial-Audio           host backend: %s\n", got);
    } else {
        std::fprintf(stderr,
            "I/Cordial-Audio           host backend: %s (CORDIAL_AUDIO_HOST=%s)\n",
            got, asked);
    }
}

} // extern "C"
