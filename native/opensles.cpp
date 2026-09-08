// OpenSL ES, backed by PipeWire.
//
// `libOpenSLES.so` was listed in `EMPTY_LIBRARIES` on the basis that Roblox
// consulted it only through `dlsym`, if at all. That was true of the build
// Cordial was first developed against. It is not true of current builds, which
// reference eight OpenSL symbols directly:
//
//     slCreateEngine
//     SL_IID_ENGINE  SL_IID_PLAY  SL_IID_RECORD  SL_IID_VOLUME
//     SL_IID_BUFFERQUEUE  SL_IID_ANDROIDSIMPLEBUFFERQUEUE
//     SL_IID_ANDROIDCONFIGURATION
//
// Seven of those are *data*, not functions — an `SLInterfaceID` is a pointer to
// a UUID struct. A missing data symbol fails the DT_NEEDED walk outright, so
// the whole client stopped loading with:
//
//     cannot locate symbol "SL_IID_ENGINE" referenced by "libroblox.so"
//
// This file used to stop there: it provided the symbols so the library
// linked, and `slCreateEngine` reported failure — the honest answer for a
// host with no OpenSL implementation behind it. That gap is now filled for
// the one path Roblox's Android build actually exercises: an engine, one
// output mix, and audio players sourced from an
// `SL_DATALOCATOR_ANDROIDSIMPLEBUFFERQUEUE`/`SLDataFormat_PCM` pair, pushed
// to PipeWire through `pipewire_backend.h`. `slCreateEngine` still reports
// failure — now specifically when PipeWire itself is unreachable, in which
// case there genuinely is nowhere for the audio to go and pretending
// otherwise would only move that failure somewhere less legible. See
// `pipewire_backend.cpp` for how "unreachable" is decided.
//
// Recording (`SL_IID_RECORD`) is implemented here, which reverses what this
// comment said until now. The previous revision refused `CreateAudioRecorder`
// outright, arguing that Roblox records through
// `org.webrtc.voiceengine.WebRtcAudioRecord` rather than through OpenSL ES and
// that implementing a recorder would add "a second way to open the microphone
// that nothing asks for". The second half of that still holds as a principle;
// the first half was an inference from the dex declaring a WebRTC surface, and
// it does not survive `docs/analysis/undefined-symbols.tsv`: `libroblox.so`
// references `SL_IID_RECORD` as a linked data symbol. That does not prove the
// engine calls `CreateAudioRecorder` — a linked IID proves the code that
// *could* was compiled in, no more — but it does mean the OpenSL recorder is a
// path the engine has, and refusing it was closing a door on the basis that
// nobody had checked whether anyone was behind it.
//
// The privacy rule is what makes implementing it the safer answer rather than
// the riskier one. A recorder object exists from `CreateAudioRecorder` and
// survives `Realize`, and neither of those opens anything: the PipeWire capture
// stream is created by `SLRecordItf::SetRecordState(SL_RECORDSTATE_RECORDING)`
// and destroyed — not paused, not muted, destroyed — by `SL_RECORDSTATE_PAUSED`,
// `SL_RECORDSTATE_STOPPED` and `Destroy`. So the microphone's lifetime is
// exactly the engine's own recording state, which is a thing an outside
// observer can check with `pw-dump` while the client runs, and is what
// `native/audio_probe.cpp` checks. Refusing to implement the recorder would
// have made that guarantee vacuous rather than true.
//
// `WebRtcAudioRecord` in `audio_classes.cpp` is still refused, and that is not
// an inconsistency: voice chat there has no downlink (`WebRtcAudioTrack` is not
// implemented), so a microphone opened for it would be feeding a session with
// nothing to play the other side of.
//
// Device *enumeration* is likewise not here. Roblox reads
// `AudioManager.getDevices(int)` and `android.media.AudioDeviceInfo`, which are
// Java; `SLOutputMixItf::GetDestinationOutputDeviceIDs` below reports zero
// devices and that remains correct — the OpenSL routing surface is genuinely
// not where Android's device list comes from. See `audio_classes.cpp`.
//
// The vtable layouts, struct definitions and every numeric constant below
// come from Khronos's OpenSL ES 1.0.1 header and AOSP's Android extension
// headers (`SLES/OpenSLES.h`, `SLES/OpenSLES_Android.h`,
// `SLES/OpenSLES_AndroidConfiguration.h` — Khronos/Apache licensed, fetched
// from android.googlesource.com/platform/frameworks/wilhelm, never from a
// decompilation of Roblox or anything shipped inside its APK). Two are worth
// flagging because they are easy to get wrong from memory and wrong here
// silently breaks every OpenSL call: `SL_DATALOCATOR_ANDROIDSIMPLEBUFFERQUEUE`
// is `0x800007BD`, in the vendor-extension range, not a small sequential
// value; and `SL_RESULT_FEATURE_UNSUPPORTED` is `12` (`0x0000000C`) — the
// previous revision of this file returned `7`, which is actually
// `SL_RESULT_BUFFER_INSUFFICIENT`. Roblox evidently only ever checked
// `slCreateEngine`'s result for "not success", so that mistake had no
// observed effect, but it was wrong and is fixed below.
//
// The IID globals remain distinct, non-null pointers to zeroed structs, and
// every interface comparison in this file is by pointer identity — the
// pattern AOSP's own OpenSL ES implementation uses, and the only one that
// does not require this file to reproduce Khronos's actual UUID byte values.

#include <chrono>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <atomic>
#include <deque>
#include <mutex>
#include <string>
#include <thread>

#include "pipewire_backend.h"

namespace {

// -------------------------------------------------------------- SL spec types
//
// Only the subset OpenSL ES 1.0.1 and its Android extensions actually need
// for engine/output-mix/audio-player objects — not a general-purpose header.

using SLuint8 = uint8_t;
using SLint16 = int16_t;
using SLuint16 = uint16_t;
using SLint32 = int32_t;
using SLuint32 = uint32_t;
using SLboolean = SLuint32;
using SLchar = SLuint8;
using SLmillibel = SLint16;
using SLmillisecond = SLuint32;
using SLpermille = SLint16;
using SLresult = SLuint32;

constexpr SLboolean SL_BOOLEAN_FALSE = 0;
constexpr SLboolean SL_BOOLEAN_TRUE = 1;

constexpr SLresult SL_RESULT_SUCCESS = 0x00000000;
constexpr SLresult SL_RESULT_PARAMETER_INVALID = 0x00000002;
constexpr SLresult SL_RESULT_RESOURCE_ERROR = 0x00000004;
constexpr SLresult SL_RESULT_BUFFER_INSUFFICIENT = 0x00000007;
constexpr SLresult SL_RESULT_CONTENT_UNSUPPORTED = 0x00000009;
// See the file header: this is 12, not 7.
constexpr SLresult SL_RESULT_FEATURE_UNSUPPORTED = 0x0000000C;

constexpr SLuint32 SL_OBJECT_STATE_UNREALIZED = 0x00000001;
constexpr SLuint32 SL_OBJECT_STATE_REALIZED = 0x00000002;

constexpr SLint32 SL_PRIORITY_NORMAL = 0x00000000;

constexpr SLmillibel SL_MILLIBEL_MAX = 0x7FFF;
constexpr SLmillibel SL_MILLIBEL_MIN = -SL_MILLIBEL_MAX - 1;

constexpr SLuint32 SL_DATALOCATOR_IODEVICE = 0x00000003;
constexpr SLuint32 SL_DATALOCATOR_OUTPUTMIX = 0x00000004;
// Android extension range; see the file header comment.
constexpr SLuint32 SL_DATALOCATOR_ANDROIDSIMPLEBUFFERQUEUE = 0x800007BD;

constexpr SLuint32 SL_IODEVICE_AUDIOINPUT = 0x00000001;
constexpr SLuint32 SL_DEFAULTDEVICEID_AUDIOINPUT = 0xFFFFFFFF;

constexpr SLuint32 SL_DATAFORMAT_PCM = 0x00000002;
constexpr SLuint32 SL_ANDROID_DATAFORMAT_PCM_EX = 0x00000004;
constexpr SLuint32 SL_ANDROID_PCM_REPRESENTATION_SIGNED_INT = 0x00000001;

constexpr SLuint32 SL_BYTEORDER_BIGENDIAN = 0x00000001;

constexpr SLuint32 SL_PLAYSTATE_STOPPED = 0x00000001;
constexpr SLuint32 SL_PLAYSTATE_PAUSED = 0x00000002;
constexpr SLuint32 SL_PLAYSTATE_PLAYING = 0x00000003;

constexpr SLuint32 SL_RECORDSTATE_STOPPED = 0x00000001;
constexpr SLuint32 SL_RECORDSTATE_PAUSED = 0x00000002;
constexpr SLuint32 SL_RECORDSTATE_RECORDING = 0x00000003;

constexpr SLuint32 SL_TIME_UNKNOWN = 0xFFFFFFFF;

/// `SLInterfaceID_` from the spec: a 128-bit UUID. Laid out here so each
/// exported ID is a distinct object of the right size, not so its value
/// carries meaning — see the file header on identity-based comparison.
struct InterfaceID {
    uint32_t time_low;
    uint16_t time_mid;
    uint16_t time_hi_and_version;
    uint16_t clock_seq;
    uint8_t node[6];
};
using SLInterfaceID = InterfaceID*;

struct SLDataSource {
    void* pLocator;
    void* pFormat;
};
using SLDataSink = SLDataSource;

struct SLDataLocator_OutputMix {
    SLuint32 locatorType;
    void* outputMix; // SLObjectItf, kept opaque here: never dereferenced, only checked non-null
};

struct SLDataLocator_AndroidSimpleBufferQueue {
    SLuint32 locatorType;
    SLuint32 numBuffers;
};

/// The source side of an audio recorder: which input device the recording
/// comes from. `device` is an `SLObjectItf` naming a specific IO device
/// object; Android's own recorders leave it null and set `deviceID` to
/// `SL_DEFAULTDEVICEID_AUDIOINPUT`, which is the only shape accepted below.
struct SLDataLocator_IODevice {
    SLuint32 locatorType;
    SLuint32 deviceType;
    SLuint32 deviceID;
    void* device; // SLObjectItf, never dereferenced here
};

/// Classic `SLDataFormat_PCM` (7 `SLuint32` fields) and the Android
/// `SLAndroidDataFormat_PCM_EX` extension (the same 7 plus a `representation`
/// field) share a layout for as many fields as the shorter one has. Reading
/// the `representation` field through this struct when `formatType` says
/// classic PCM would read one `SLuint32` past what Roblox actually
/// allocated — the two are kept as separate types below and `formatType` is
/// read (and only that field) before deciding which one applies.
struct SLDataFormat_PCM {
    SLuint32 formatType;
    SLuint32 numChannels;
    SLuint32 samplesPerSec; // milliHertz: 44100000 means 44.1kHz
    SLuint32 bitsPerSample;
    SLuint32 containerSize;
    SLuint32 channelMask;
    SLuint32 endianness;
};

struct SLAndroidDataFormat_PCM_EX {
    SLuint32 formatType;
    SLuint32 numChannels;
    SLuint32 sampleRate; // same milliHertz units, different field name
    SLuint32 bitsPerSample;
    SLuint32 containerSize;
    SLuint32 channelMask;
    SLuint32 endianness;
    SLuint32 representation;
};

// ------------------------------------------------------------- SLObjectItf

struct SLObjectItf_;
using SLObjectItf = const SLObjectItf_* const*;

struct SLObjectItf_ {
    SLresult (*Realize)(SLObjectItf self, SLboolean async);
    SLresult (*Resume)(SLObjectItf self, SLboolean async);
    SLresult (*GetState)(SLObjectItf self, SLuint32* pState);
    SLresult (*GetInterface)(SLObjectItf self, SLInterfaceID iid, void* pInterface);
    SLresult (*RegisterCallback)(SLObjectItf self, void* callback, void* pContext);
    void (*AbortAsyncOperation)(SLObjectItf self);
    void (*Destroy)(SLObjectItf self);
    SLresult (*SetPriority)(SLObjectItf self, SLint32 priority, SLboolean preemptable);
    SLresult (*GetPriority)(SLObjectItf self, SLint32* pPriority, SLboolean* pPreemptable);
    SLresult (*SetLossOfControlInterfaces)(SLObjectItf self, SLint16 numInterfaces,
                                            SLInterfaceID* pInterfaceIDs, SLboolean enabled);
};

// ------------------------------------------------------------ SLOutputMixItf

struct SLOutputMixItf_;
using SLOutputMixItf = const SLOutputMixItf_* const*;

struct SLOutputMixItf_ {
    SLresult (*GetDestinationOutputDeviceIDs)(SLOutputMixItf self, SLint32* pNumDevices,
                                               SLuint32* pDeviceIDs);
    SLresult (*RegisterDeviceChangeCallback)(SLOutputMixItf self, void* callback, void* pContext);
    SLresult (*ReRoute)(SLOutputMixItf self, SLint32 numOutputDevices, SLuint32* pOutputDeviceIDs);
};

// ----------------------------------------------------------------- SLPlayItf

struct SLPlayItf_;
using SLPlayItf = const SLPlayItf_* const*;

struct SLPlayItf_ {
    SLresult (*SetPlayState)(SLPlayItf self, SLuint32 state);
    SLresult (*GetPlayState)(SLPlayItf self, SLuint32* pState);
    SLresult (*GetDuration)(SLPlayItf self, SLmillisecond* pMsec);
    SLresult (*GetPosition)(SLPlayItf self, SLmillisecond* pMsec);
    SLresult (*RegisterCallback)(SLPlayItf self, void* callback, void* pContext);
    SLresult (*SetCallbackEventsMask)(SLPlayItf self, SLuint32 eventFlags);
    SLresult (*GetCallbackEventsMask)(SLPlayItf self, SLuint32* pEventFlags);
    SLresult (*SetMarkerPosition)(SLPlayItf self, SLmillisecond mSec);
    SLresult (*ClearMarkerPosition)(SLPlayItf self);
    SLresult (*GetMarkerPosition)(SLPlayItf self, SLmillisecond* pMsec);
    SLresult (*SetPositionUpdatePeriod)(SLPlayItf self, SLmillisecond mSec);
    SLresult (*GetPositionUpdatePeriod)(SLPlayItf self, SLmillisecond* pMsec);
};

// --------------------------------------------------------------- SLRecordItf
//
// Deliberately the same shape as `SLPlayItf_` one field at a time, because it
// is: the spec defines the two symmetrically, and a recorder's state machine
// is the player's with `SetPlayState` renamed. The one asymmetry that matters
// here is not in the vtable at all — see `record_SetRecordState`, where a
// state change is what creates and destroys a microphone.

struct SLRecordItf_;
using SLRecordItf = const SLRecordItf_* const*;

struct SLRecordItf_ {
    SLresult (*SetRecordState)(SLRecordItf self, SLuint32 state);
    SLresult (*GetRecordState)(SLRecordItf self, SLuint32* pState);
    SLresult (*SetDurationLimit)(SLRecordItf self, SLmillisecond msec);
    SLresult (*GetPosition)(SLRecordItf self, SLmillisecond* pMsec);
    SLresult (*RegisterCallback)(SLRecordItf self, void* callback, void* pContext);
    SLresult (*SetCallbackEventsMask)(SLRecordItf self, SLuint32 eventFlags);
    SLresult (*GetCallbackEventsMask)(SLRecordItf self, SLuint32* pEventFlags);
    SLresult (*SetMarkerPosition)(SLRecordItf self, SLmillisecond mSec);
    SLresult (*ClearMarkerPosition)(SLRecordItf self);
    SLresult (*GetMarkerPosition)(SLRecordItf self, SLmillisecond* pMsec);
    SLresult (*SetPositionUpdatePeriod)(SLRecordItf self, SLmillisecond mSec);
    SLresult (*GetPositionUpdatePeriod)(SLRecordItf self, SLmillisecond* pMsec);
};

// --------------------------------------------------------------- SLVolumeItf

struct SLVolumeItf_;
using SLVolumeItf = const SLVolumeItf_* const*;

struct SLVolumeItf_ {
    SLresult (*SetVolumeLevel)(SLVolumeItf self, SLmillibel level);
    SLresult (*GetVolumeLevel)(SLVolumeItf self, SLmillibel* pLevel);
    SLresult (*GetMaxVolumeLevel)(SLVolumeItf self, SLmillibel* pMaxLevel);
    SLresult (*SetMute)(SLVolumeItf self, SLboolean mute);
    SLresult (*GetMute)(SLVolumeItf self, SLboolean* pMute);
    SLresult (*EnableStereoPosition)(SLVolumeItf self, SLboolean enable);
    SLresult (*IsEnabledStereoPosition)(SLVolumeItf self, SLboolean* pEnable);
    SLresult (*SetStereoPosition)(SLVolumeItf self, SLpermille stereoPosition);
    SLresult (*GetStereoPosition)(SLVolumeItf self, SLpermille* pStereoPosition);
};

// ------------------------------------------------ SLAndroidSimpleBufferQueueItf

struct SLAndroidSimpleBufferQueueItf_;
using SLAndroidSimpleBufferQueueItf = const SLAndroidSimpleBufferQueueItf_* const*;
using slAndroidSimpleBufferQueueCallback = void (*)(SLAndroidSimpleBufferQueueItf caller,
                                                      void* pContext);

struct SLAndroidSimpleBufferQueueState {
    SLuint32 count;
    SLuint32 index;
};

struct SLAndroidSimpleBufferQueueItf_ {
    SLresult (*Enqueue)(SLAndroidSimpleBufferQueueItf self, const void* pBuffer, SLuint32 size);
    SLresult (*Clear)(SLAndroidSimpleBufferQueueItf self);
    SLresult (*GetState)(SLAndroidSimpleBufferQueueItf self, SLAndroidSimpleBufferQueueState* pState);
    SLresult (*RegisterCallback)(SLAndroidSimpleBufferQueueItf self,
                                  slAndroidSimpleBufferQueueCallback callback, void* pContext);
};

// -------------------------------------------------------- SLAndroidConfigurationItf

struct SLAndroidConfigurationItf_;
using SLAndroidConfigurationItf = const SLAndroidConfigurationItf_* const*;

struct SLAndroidConfigurationItf_ {
    SLresult (*SetConfiguration)(SLAndroidConfigurationItf self, const SLchar* configKey,
                                  const void* pConfigValue, SLuint32 valueSize);
    SLresult (*GetConfiguration)(SLAndroidConfigurationItf self, const SLchar* configKey,
                                  SLuint32* pValueSize, void* pConfigValue);
    // `jobject` in the real header; kept as `void**` here (same pointer-sized
    // opaque handle) so this file has no dependency on a JNI header.
    SLresult (*AcquireJavaProxy)(SLAndroidConfigurationItf self, SLuint32 proxyType, void** pProxyObj);
    SLresult (*ReleaseJavaProxy)(SLAndroidConfigurationItf self, SLuint32 proxyType);
};

// --------------------------------------------------------------- SLEngineItf

struct SLEngineItf_;
using SLEngineItf = const SLEngineItf_* const*;

struct SLEngineItf_ {
    SLresult (*CreateLEDDevice)(SLEngineItf self, SLObjectItf* pDevice, SLuint32 deviceID,
                                 SLuint32 numInterfaces, const SLInterfaceID* pInterfaceIds,
                                 const SLboolean* pInterfaceRequired);
    SLresult (*CreateVibraDevice)(SLEngineItf self, SLObjectItf* pDevice, SLuint32 deviceID,
                                   SLuint32 numInterfaces, const SLInterfaceID* pInterfaceIds,
                                   const SLboolean* pInterfaceRequired);
    SLresult (*CreateAudioPlayer)(SLEngineItf self, SLObjectItf* pPlayer, SLDataSource* pAudioSrc,
                                   SLDataSink* pAudioSnk, SLuint32 numInterfaces,
                                   const SLInterfaceID* pInterfaceIds,
                                   const SLboolean* pInterfaceRequired);
    SLresult (*CreateAudioRecorder)(SLEngineItf self, SLObjectItf* pRecorder, SLDataSource* pAudioSrc,
                                     SLDataSink* pAudioSnk, SLuint32 numInterfaces,
                                     const SLInterfaceID* pInterfaceIds,
                                     const SLboolean* pInterfaceRequired);
    SLresult (*CreateMidiPlayer)(SLEngineItf self, SLObjectItf* pPlayer, SLDataSource* pMIDISrc,
                                  SLDataSource* pBankSrc, SLDataSink* pAudioOutput, SLDataSink* pVibra,
                                  SLDataSink* pLEDArray, SLuint32 numInterfaces,
                                  const SLInterfaceID* pInterfaceIds,
                                  const SLboolean* pInterfaceRequired);
    SLresult (*CreateListener)(SLEngineItf self, SLObjectItf* pListener, SLuint32 numInterfaces,
                                const SLInterfaceID* pInterfaceIds,
                                const SLboolean* pInterfaceRequired);
    SLresult (*Create3DGroup)(SLEngineItf self, SLObjectItf* pGroup, SLuint32 numInterfaces,
                               const SLInterfaceID* pInterfaceIds,
                               const SLboolean* pInterfaceRequired);
    SLresult (*CreateOutputMix)(SLEngineItf self, SLObjectItf* pMix, SLuint32 numInterfaces,
                                 const SLInterfaceID* pInterfaceIds,
                                 const SLboolean* pInterfaceRequired);
    SLresult (*CreateMetadataExtractor)(SLEngineItf self, SLObjectItf* pMetadataExtractor,
                                         SLDataSource* pDataSource, SLuint32 numInterfaces,
                                         const SLInterfaceID* pInterfaceIds,
                                         const SLboolean* pInterfaceRequired);
    SLresult (*CreateExtensionObject)(SLEngineItf self, SLObjectItf* pObject, void* pParameters,
                                       SLuint32 objectID, SLuint32 numInterfaces,
                                       const SLInterfaceID* pInterfaceIds,
                                       const SLboolean* pInterfaceRequired);
    SLresult (*QueryNumSupportedInterfaces)(SLEngineItf self, SLuint32 objectID,
                                             SLuint32* pNumSupportedInterfaces);
    SLresult (*QuerySupportedInterfaces)(SLEngineItf self, SLuint32 objectID, SLuint32 index,
                                          SLInterfaceID* pInterfaceId);
    SLresult (*QueryNumSupportedExtensions)(SLEngineItf self, SLuint32* pNumExtensions);
    SLresult (*QuerySupportedExtension)(SLEngineItf self, SLuint32 index, SLchar* pExtensionName,
                                         SLint16* pNameLength);
    SLresult (*IsExtensionSupported)(SLEngineItf self, const SLchar* pExtensionName,
                                      SLboolean* pSupported);
};

// ----------------------------------------------------------------- IID globals

InterfaceID g_engine{};
InterfaceID g_play{};
InterfaceID g_record{};
InterfaceID g_volume{};
InterfaceID g_buffer_queue{};
InterfaceID g_android_simple_buffer_queue{};
InterfaceID g_android_configuration{};
// Not exported: Roblox never references SL_IID_OUTPUTMIX as a linked symbol
// (see the eight-symbol list in the file header, which does not include it),
// so nothing outside this file can ever hold this pointer to compare against
// it. Kept only so CreateOutputMix's own interface-request validation has a
// real identity to check, in case that ever changes.
InterfaceID g_output_mix{};

} // namespace

extern "C" {
InterfaceID* SL_IID_ENGINE = &g_engine;
InterfaceID* SL_IID_PLAY = &g_play;
InterfaceID* SL_IID_RECORD = &g_record;
InterfaceID* SL_IID_VOLUME = &g_volume;
InterfaceID* SL_IID_BUFFERQUEUE = &g_buffer_queue;
InterfaceID* SL_IID_ANDROIDSIMPLEBUFFERQUEUE = &g_android_simple_buffer_queue;
InterfaceID* SL_IID_ANDROIDCONFIGURATION = &g_android_configuration;
} // extern "C"

namespace {

// ------------------------------------------------------------- object model
//
// Every OpenSL "object" here is a plain struct whose first field is a
// pointer to a static `SLObjectItf_` vtable. That is what makes the spec's
// own `self`-recovery trick work: `SLObjectItf` is defined by the spec as a
// pointer *to* the vtable-pointer field, so a method that receives `self`
// can `reinterpret_cast` it straight back to the concrete struct exactly
// when the vtable pointer is that struct's first member — which it always
// is here. Interfaces further into the struct (an audio player's `SLPlayItf`
// is its second field) use `offsetof` to walk back to the same struct
// instead; see `player_from_play` and its siblings below.
//
// Each concrete kind gets its own `SLObjectItf_` vtable instance rather than
// one shared table with a run-time type tag: static dispatch through the
// pointer that was installed at construction time is simpler and cannot
// diverge from what was actually constructed.

/// `self` arrives typed as the spec's `const ...Itf_* const*`, but it always
/// points at a plain mutable field inside one of the objects below — Roblox
/// never holds a const view of its own object. `reinterpret_cast` alone
/// cannot drop that const (the standard requires `const_cast` for that
/// step), so every accessor below goes through this rather than repeating
/// the two-cast dance inline.
template <typename Concrete, typename Itf>
Concrete* strip_const(Itf self) {
    return reinterpret_cast<Concrete*>(const_cast<void*>(static_cast<const void*>(self)));
}

SLresult object_RegisterCallback(SLObjectItf, void*, void*) { return SL_RESULT_SUCCESS; }
void object_AbortAsyncOperation(SLObjectItf) {}
SLresult object_SetPriority(SLObjectItf, SLint32, SLboolean) { return SL_RESULT_SUCCESS; }
SLresult object_GetPriority(SLObjectItf, SLint32* pPriority, SLboolean* pPreemptable) {
    if (pPriority) *pPriority = SL_PRIORITY_NORMAL;
    if (pPreemptable) *pPreemptable = SL_BOOLEAN_TRUE;
    return SL_RESULT_SUCCESS;
}
SLresult object_SetLossOfControlInterfaces(SLObjectItf, SLint16, SLInterfaceID*, SLboolean) {
    return SL_RESULT_SUCCESS;
}
SLresult object_Resume(SLObjectItf, SLboolean) {
    return SL_RESULT_SUCCESS; // nothing here ever suspends, so there is nothing to resume from
}

// --------------------------------------------------------------- EngineObject

struct EngineObject {
    const SLObjectItf_* objectVtable;
    const SLEngineItf_* engineVtable;
    std::atomic<SLuint32> state{SL_OBJECT_STATE_UNREALIZED};
};

SLresult engine_Realize(SLObjectItf self, SLboolean) {
    strip_const<EngineObject>(self)->state.store(SL_OBJECT_STATE_REALIZED);
    return SL_RESULT_SUCCESS; // PipeWire reachability was already confirmed by slCreateEngine
}
SLresult engine_GetState(SLObjectItf self, SLuint32* pState) {
    if (!pState) return SL_RESULT_PARAMETER_INVALID;
    *pState = strip_const<EngineObject>(self)->state.load();
    return SL_RESULT_SUCCESS;
}
SLresult engine_GetInterface(SLObjectItf self, SLInterfaceID iid, void* pInterface) {
    if (!pInterface) return SL_RESULT_PARAMETER_INVALID;
    auto* e = strip_const<EngineObject>(self);
    if (iid == SL_IID_ENGINE) {
        *reinterpret_cast<const void**>(pInterface) = &e->engineVtable;
        return SL_RESULT_SUCCESS;
    }
    return SL_RESULT_FEATURE_UNSUPPORTED;
}
void engine_Destroy(SLObjectItf self) {
    auto* e = strip_const<EngineObject>(self);
    e->state.store(SL_OBJECT_STATE_UNREALIZED);
    delete e;
}

// ------------------------------------------------------------ OutputMixObject

struct OutputMixObject {
    const SLObjectItf_* objectVtable;
    const SLOutputMixItf_* outputMixVtable;
    std::atomic<SLuint32> state{SL_OBJECT_STATE_UNREALIZED};
};

SLresult outputmix_GetDestinationOutputDeviceIDs(SLOutputMixItf, SLint32* pNumDevices, SLuint32*) {
    // No per-device routing surface: this backend has exactly one
    // destination (the shared PipeWire session), chosen by PipeWire's own
    // session manager rather than by device ID.
    if (pNumDevices) *pNumDevices = 0;
    return SL_RESULT_SUCCESS;
}
SLresult outputmix_RegisterDeviceChangeCallback(SLOutputMixItf, void*, void*) {
    return SL_RESULT_SUCCESS; // accepted; never fires, because device changes are never surfaced above
}
SLresult outputmix_ReRoute(SLOutputMixItf, SLint32, SLuint32*) {
    return SL_RESULT_FEATURE_UNSUPPORTED;
}

SLresult outputmixobj_Realize(SLObjectItf self, SLboolean) {
    strip_const<OutputMixObject>(self)->state.store(SL_OBJECT_STATE_REALIZED);
    return SL_RESULT_SUCCESS;
}
SLresult outputmixobj_GetState(SLObjectItf self, SLuint32* pState) {
    if (!pState) return SL_RESULT_PARAMETER_INVALID;
    *pState = strip_const<OutputMixObject>(self)->state.load();
    return SL_RESULT_SUCCESS;
}
SLresult outputmixobj_GetInterface(SLObjectItf self, SLInterfaceID iid, void* pInterface) {
    if (!pInterface) return SL_RESULT_PARAMETER_INVALID;
    auto* m = strip_const<OutputMixObject>(self);
    if (iid == &g_output_mix) {
        *reinterpret_cast<const void**>(pInterface) = &m->outputMixVtable;
        return SL_RESULT_SUCCESS;
    }
    return SL_RESULT_FEATURE_UNSUPPORTED;
}
void outputmixobj_Destroy(SLObjectItf self) {
    auto* m = strip_const<OutputMixObject>(self);
    m->state.store(SL_OBJECT_STATE_UNREALIZED);
    delete m;
}

// ---------------------------------------------------------- AudioPlayerObject

struct AudioPlayerObject {
    const SLObjectItf_* objectVtable;
    const SLPlayItf_* playVtable;
    const SLAndroidSimpleBufferQueueItf_* bufferQueueVtable;
    const SLVolumeItf_* volumeVtable;
    const SLAndroidConfigurationItf_* androidConfigVtable;

    std::atomic<SLuint32> state{SL_OBJECT_STATE_UNREALIZED};
    std::atomic<SLuint32> playState{SL_PLAYSTATE_STOPPED};

    cordial::audio::PlaybackStream stream;

    // Format, captured at CreateAudioPlayer time from the caller's
    // SLDataFormat_PCM and used to open `stream` at Realize.
    uint32_t rateHz = 0;
    uint32_t channels = 0;
    uint32_t bitsPerSample = 0;
    uint32_t containerBits = 0;
    bool bigEndian = false;
    uint32_t numBuffers = 2;

    std::mutex callbackMutex;
    slAndroidSimpleBufferQueueCallback callback = nullptr;
    void* callbackContext = nullptr;

    std::atomic<float> volumeLinear{1.0f};
    std::atomic<bool> muted{false};

    // SetMarkerPosition/SetPositionUpdatePeriod: accepted and echoed back
    // faithfully (see play_SetMarkerPosition), but the marker/period events
    // they configure are never emitted — this player does not track
    // playback position, so there is no moment to compare against them.
    std::atomic<SLmillisecond> markerPositionMs{0};
    std::atomic<SLmillisecond> positionUpdatePeriodMs{0};
};

/// Invoked from PipeWire's thread (via `pipewire_backend.cpp`'s `process()`)
/// once a buffer this player enqueued has been fully copied out. Forwards to
/// whatever `slAndroidSimpleBufferQueueCallback` Roblox last registered,
/// exactly as `SLAndroidSimpleBufferQueueItf::RegisterCallback` documents:
/// one callback, one caller-supplied context, invoked with `self` as the
/// buffer-queue interface pointer.
void on_buffer_drained(void* /*buffer_context*/, void* user) {
    auto* p = static_cast<AudioPlayerObject*>(user);
    slAndroidSimpleBufferQueueCallback cb;
    void* ctx;
    {
        std::lock_guard<std::mutex> lock(p->callbackMutex);
        cb = p->callback;
        ctx = p->callbackContext;
    }
    if (cb) {
        cb(reinterpret_cast<SLAndroidSimpleBufferQueueItf>(&p->bufferQueueVtable), ctx);
    }
}

SLresult player_Realize(SLObjectItf self, SLboolean) {
    auto* p = strip_const<AudioPlayerObject>(self);
    // The third output path, and it honours the user's sink for the same
    // reason the other two do: FMOD reaches PipeWire through whichever of
    // OpenSL ES, AAudio and the Java `AudioDevice` it settles on, and a device
    // choice that only applied to two of the three would be a setting that
    // works or does not depending on a decision the user never sees.
    if (!p->stream.open(p->rateHz, p->channels, p->bitsPerSample, p->containerBits, p->bigEndian,
                         p->numBuffers, cordial::audio::configured_output_device())) {
        // pipewire_backend.cpp already printed the specific reason (unsupported
        // layout, or the stream failed to connect).
        return SL_RESULT_CONTENT_UNSUPPORTED;
    }
    p->stream.set_drain_callback(&on_buffer_drained, p);
    p->stream.set_volume_linear(p->volumeLinear.load());
    p->stream.set_mute(p->muted.load());
    p->state.store(SL_OBJECT_STATE_REALIZED);
    return SL_RESULT_SUCCESS;
}
SLresult player_GetState(SLObjectItf self, SLuint32* pState) {
    if (!pState) return SL_RESULT_PARAMETER_INVALID;
    *pState = strip_const<AudioPlayerObject>(self)->state.load();
    return SL_RESULT_SUCCESS;
}
SLresult player_GetInterface(SLObjectItf self, SLInterfaceID iid, void* pInterface) {
    if (!pInterface) return SL_RESULT_PARAMETER_INVALID;
    auto* p = strip_const<AudioPlayerObject>(self);
    if (iid == SL_IID_PLAY) {
        *reinterpret_cast<const void**>(pInterface) = &p->playVtable;
        return SL_RESULT_SUCCESS;
    }
    if (iid == SL_IID_ANDROIDSIMPLEBUFFERQUEUE) {
        *reinterpret_cast<const void**>(pInterface) = &p->bufferQueueVtable;
        return SL_RESULT_SUCCESS;
    }
    if (iid == SL_IID_VOLUME) {
        *reinterpret_cast<const void**>(pInterface) = &p->volumeVtable;
        return SL_RESULT_SUCCESS;
    }
    if (iid == SL_IID_ANDROIDCONFIGURATION) {
        *reinterpret_cast<const void**>(pInterface) = &p->androidConfigVtable;
        return SL_RESULT_SUCCESS;
    }
    // Includes SL_IID_BUFFERQUEUE deliberately: Roblox links against it (see
    // the file header), but Android's own audio players use the
    // ANDROIDSIMPLEBUFFERQUEUE extension, not the generic one, so nothing
    // here ever needs to hand one out.
    return SL_RESULT_FEATURE_UNSUPPORTED;
}
void player_Destroy(SLObjectItf self) {
    auto* p = strip_const<AudioPlayerObject>(self);
    p->stream.set_active(false);
    p->stream.close();
    p->state.store(SL_OBJECT_STATE_UNREALIZED);
    delete p;
}

AudioPlayerObject* player_from_play(SLPlayItf self) {
    return reinterpret_cast<AudioPlayerObject*>(strip_const<char>(self) -
                                                 offsetof(AudioPlayerObject, playVtable));
}

SLresult play_SetPlayState(SLPlayItf self, SLuint32 state) {
    auto* p = player_from_play(self);
    switch (state) {
        case SL_PLAYSTATE_PLAYING:
            p->stream.set_active(true);
            break;
        case SL_PLAYSTATE_PAUSED:
            p->stream.set_active(false);
            break;
        case SL_PLAYSTATE_STOPPED:
            p->stream.set_active(false);
            p->stream.clear();
            break;
        default:
            return SL_RESULT_PARAMETER_INVALID;
    }
    p->playState.store(state);
    return SL_RESULT_SUCCESS;
}
SLresult play_GetPlayState(SLPlayItf self, SLuint32* pState) {
    if (!pState) return SL_RESULT_PARAMETER_INVALID;
    *pState = player_from_play(self)->playState.load();
    return SL_RESULT_SUCCESS;
}
SLresult play_GetDuration(SLPlayItf, SLmillisecond* pMsec) {
    // Spec-correct for a buffer-queue source: there is no inherent duration,
    // the queue is fed indefinitely.
    if (pMsec) *pMsec = SL_TIME_UNKNOWN;
    return SL_RESULT_SUCCESS;
}
SLresult play_GetPosition(SLPlayItf, SLmillisecond* pMsec) {
    // Not tracked: doing so honestly would mean querying PipeWire's own
    // pw_stream_get_time() and translating its graph-clock ticks back to
    // milliseconds of this player's queue, which nothing here has needed
    // yet. Zero rather than a fabricated estimate.
    if (pMsec) *pMsec = 0;
    return SL_RESULT_SUCCESS;
}
SLresult play_RegisterCallback(SLPlayItf, void*, void*) {
    return SL_RESULT_SUCCESS; // accepted; see SetCallbackEventsMask on why it can never fire
}
SLresult play_SetCallbackEventsMask(SLPlayItf, SLuint32) {
    // Every SL_PLAYEVENT_* (head-at-end, at-marker, at-new-pos, moving,
    // stalled) needs position tracking this player does not do. Accepting
    // the mask instead of rejecting it matches SLAndroidConfigurationItf's
    // reasoning: registering interest in an event that structurally never
    // occurs is not the same claim as promising the event will fire.
    return SL_RESULT_SUCCESS;
}
SLresult play_GetCallbackEventsMask(SLPlayItf, SLuint32* pEventFlags) {
    if (pEventFlags) *pEventFlags = 0;
    return SL_RESULT_SUCCESS;
}
SLresult play_SetMarkerPosition(SLPlayItf self, SLmillisecond mSec) {
    player_from_play(self)->markerPositionMs.store(mSec);
    return SL_RESULT_SUCCESS;
}
SLresult play_ClearMarkerPosition(SLPlayItf self) {
    player_from_play(self)->markerPositionMs.store(0);
    return SL_RESULT_SUCCESS;
}
SLresult play_GetMarkerPosition(SLPlayItf self, SLmillisecond* pMsec) {
    if (pMsec) *pMsec = player_from_play(self)->markerPositionMs.load();
    return SL_RESULT_SUCCESS;
}
SLresult play_SetPositionUpdatePeriod(SLPlayItf self, SLmillisecond mSec) {
    player_from_play(self)->positionUpdatePeriodMs.store(mSec);
    return SL_RESULT_SUCCESS;
}
SLresult play_GetPositionUpdatePeriod(SLPlayItf self, SLmillisecond* pMsec) {
    if (pMsec) *pMsec = player_from_play(self)->positionUpdatePeriodMs.load();
    return SL_RESULT_SUCCESS;
}

AudioPlayerObject* player_from_absq(SLAndroidSimpleBufferQueueItf self) {
    return reinterpret_cast<AudioPlayerObject*>(strip_const<char>(self) -
                                                 offsetof(AudioPlayerObject, bufferQueueVtable));
}

SLresult absq_Enqueue(SLAndroidSimpleBufferQueueItf self, const void* pBuffer, SLuint32 size) {
    if (!pBuffer || size == 0) return SL_RESULT_PARAMETER_INVALID;
    // Not copied: the buffer-queue contract is that pBuffer stays valid and
    // unmodified until the drain callback fires for it, exactly as real
    // Android's implementation requires — copying here would silently give
    // Roblox a looser contract than the one it was compiled against.
    if (!player_from_absq(self)->stream.enqueue(pBuffer, size, nullptr)) {
        return SL_RESULT_BUFFER_INSUFFICIENT;
    }
    return SL_RESULT_SUCCESS;
}
SLresult absq_Clear(SLAndroidSimpleBufferQueueItf self) {
    player_from_absq(self)->stream.clear();
    return SL_RESULT_SUCCESS;
}
SLresult absq_GetState(SLAndroidSimpleBufferQueueItf self, SLAndroidSimpleBufferQueueState* pState) {
    if (!pState) return SL_RESULT_PARAMETER_INVALID;
    auto qs = player_from_absq(self)->stream.state();
    pState->count = qs.count;
    pState->index = qs.index;
    return SL_RESULT_SUCCESS;
}
SLresult absq_RegisterCallback(SLAndroidSimpleBufferQueueItf self,
                                slAndroidSimpleBufferQueueCallback callback, void* pContext) {
    auto* p = player_from_absq(self);
    std::lock_guard<std::mutex> lock(p->callbackMutex);
    p->callback = callback;
    p->callbackContext = pContext;
    return SL_RESULT_SUCCESS;
}

AudioPlayerObject* player_from_volume(SLVolumeItf self) {
    return reinterpret_cast<AudioPlayerObject*>(strip_const<char>(self) -
                                                 offsetof(AudioPlayerObject, volumeVtable));
}

SLresult volume_SetVolumeLevel(SLVolumeItf self, SLmillibel level) {
    auto* p = player_from_volume(self);
    float linear = level <= SL_MILLIBEL_MIN ? 0.0f : std::pow(10.0f, static_cast<float>(level) / 2000.0f);
    p->volumeLinear.store(linear);
    p->stream.set_volume_linear(linear);
    return SL_RESULT_SUCCESS;
}
SLresult volume_GetVolumeLevel(SLVolumeItf self, SLmillibel* pLevel) {
    if (!pLevel) return SL_RESULT_PARAMETER_INVALID;
    float linear = player_from_volume(self)->volumeLinear.load();
    if (linear <= 0.0f) {
        *pLevel = SL_MILLIBEL_MIN;
    } else {
        // **`double`, and the reason is packaging rather than precision.**
        // glibc 2.43 added a correctly-rounded `log10f` and made it the
        // default binding, so building this line against a 2.43 host emitted
        // `log10f@GLIBC_2.43` -- and that one symbol, the only one in either
        // binary above 2.39, made the `.rpm` refuse to install anywhere older
        // with "nothing provides libm.so.6(GLIBC_2.43)(64bit)". It would have
        // done the same to the AppImage, which bundles no libc and is supposed
        // to be the portable one.
        //
        // `log10` has been `GLIBC_2.2.5` since before any of this and there is
        // no version of glibc without it. The result is cast to an integer
        // millibel two lines down, so nothing here wanted single precision in
        // the first place.
        double mb = 2000.0 * std::log10(static_cast<double>(linear));
        *pLevel = static_cast<SLmillibel>(std::max<double>(SL_MILLIBEL_MIN, std::min<double>(SL_MILLIBEL_MAX, mb)));
    }
    return SL_RESULT_SUCCESS;
}
SLresult volume_GetMaxVolumeLevel(SLVolumeItf, SLmillibel* pMaxLevel) {
    if (pMaxLevel) *pMaxLevel = 0; // 0 millibel = unity gain, the ceiling PipeWire's own control expects
    return SL_RESULT_SUCCESS;
}
SLresult volume_SetMute(SLVolumeItf self, SLboolean mute) {
    auto* p = player_from_volume(self);
    bool m = mute != SL_BOOLEAN_FALSE;
    p->muted.store(m);
    p->stream.set_mute(m);
    return SL_RESULT_SUCCESS;
}
SLresult volume_GetMute(SLVolumeItf self, SLboolean* pMute) {
    if (!pMute) return SL_RESULT_PARAMETER_INVALID;
    *pMute = player_from_volume(self)->muted.load() ? SL_BOOLEAN_TRUE : SL_BOOLEAN_FALSE;
    return SL_RESULT_SUCCESS;
}
SLresult volume_EnableStereoPosition(SLVolumeItf, SLboolean) { return SL_RESULT_FEATURE_UNSUPPORTED; }
SLresult volume_IsEnabledStereoPosition(SLVolumeItf, SLboolean* pEnable) {
    if (pEnable) *pEnable = SL_BOOLEAN_FALSE;
    return SL_RESULT_SUCCESS;
}
SLresult volume_SetStereoPosition(SLVolumeItf, SLpermille) { return SL_RESULT_FEATURE_UNSUPPORTED; }
SLresult volume_GetStereoPosition(SLVolumeItf, SLpermille* pStereoPosition) {
    if (pStereoPosition) *pStereoPosition = 0;
    return SL_RESULT_SUCCESS;
}

SLresult androidconfig_SetConfiguration(SLAndroidConfigurationItf, const SLchar* configKey,
                                         const void* pConfigValue, SLuint32 valueSize) {
    // Every key defined for this interface (stream type, performance mode,
    // recording preset) is a hint about which physical Android audio path
    // or scheduling class to use. None of that exists on this host — the
    // request genuinely does not apply, which is different from an
    // unimplemented feature — so accepting and discarding is the correct
    // translation, not a shortcut.
    //
    // Discarded, but not unrecorded. Which keys Roblox sets is the cheapest
    // available evidence of how it separates voice from game audio
    // (`androidRecordingPreset` = VOICE_COMMUNICATION would say so outright),
    // and nobody has yet observed the engine reach this interface at all. One
    // line per distinct key, so a client that sets one per player does not
    // turn this into a log flood.
    if (configKey) {
        static std::mutex seen_mutex;
        static std::deque<std::string> seen;
        const std::string key(reinterpret_cast<const char*>(configKey));
        std::lock_guard<std::mutex> lock(seen_mutex);
        bool first = true;
        for (const std::string& s : seen) {
            if (s == key) { first = false; break; }
        }
        if (first) {
            seen.push_back(key);
            SLuint32 value = 0;
            if (pConfigValue && valueSize >= sizeof(SLuint32)) {
                std::memcpy(&value, pConfigValue, sizeof(SLuint32));
            }
            std::fprintf(stderr,
                "I/Cordial-OpenSLES         SLAndroidConfigurationItf::SetConfiguration('%s', "
                "%u, %u bytes) — accepted and discarded; there is no Android audio path to "
                "select on this host.\n", key.c_str(), value, valueSize);
        }
    }
    return SL_RESULT_SUCCESS;
}
SLresult androidconfig_GetConfiguration(SLAndroidConfigurationItf, const SLchar*, SLuint32* pValueSize,
                                         void*) {
    // Nothing was ever stored (see SetConfiguration above), so there is no
    // value to hand back; SL_RESULT_PARAMETER_INVALID is the spec's answer
    // for an unrecognised key, which this is indistinguishable from.
    if (pValueSize) *pValueSize = 0;
    return SL_RESULT_PARAMETER_INVALID;
}
SLresult androidconfig_AcquireJavaProxy(SLAndroidConfigurationItf, SLuint32, void** pProxyObj) {
    if (pProxyObj) *pProxyObj = nullptr;
    return SL_RESULT_FEATURE_UNSUPPORTED; // no JNI environment behind this player to proxy into
}
SLresult androidconfig_ReleaseJavaProxy(SLAndroidConfigurationItf, SLuint32) {
    return SL_RESULT_SUCCESS;
}

// -------------------------------------------------------- AudioRecorderObject
//
// The microphone, and the one object in this file whose *lifetime rules* are
// load-bearing rather than incidental. Stated once, here, because every method
// below is arranged to keep them:
//
//     CreateAudioRecorder   opens nothing
//     Realize               opens nothing
//     SetRecordState(RECORDING)  opens the capture stream
//     SetRecordState(PAUSED)     destroys it
//     SetRecordState(STOPPED)    destroys it
//     Destroy                    destroys it
//
// `Realize` opening nothing is the one that looks wrong next to
// `player_Realize`, which does open its stream. It is not symmetric on purpose:
// realizing a player is inaudible, and realizing a recorder that opened a
// capture stream would light the desktop's microphone indicator for an object
// the engine has merely constructed. The cost of the asymmetry is that a
// microphone that cannot be opened is reported at `SetRecordState` rather than
// at `Realize`, which is later than Android would report it but is the only
// place the answer can be found without holding the device to find it.
//
// Pausing destroying the stream rather than deactivating it is the same rule
// as `CaptureStream::close`'s: a paused-but-connected capture node is still a
// lit indicator and still shows every other application that Cordial holds the
// device. Samples arriving during a pause are discarded either way, so nothing
// is lost by not being there for them.

/// One caller-owned buffer waiting to be filled with captured PCM. The
/// direction is the mirror image of `PlaybackStream`'s pending list — the
/// caller supplies empty space rather than full data — but the ownership
/// contract is identical and comes from the same place: the buffer belongs to
/// the caller and must stay valid until its drain callback fires.
struct RecordBuffer {
    uint8_t* data;
    uint32_t size;
    uint32_t filled;
};

struct AudioRecorderObject {
    const SLObjectItf_* objectVtable;
    const SLRecordItf_* recordVtable;
    const SLAndroidSimpleBufferQueueItf_* bufferQueueVtable;
    const SLAndroidConfigurationItf_* androidConfigVtable;

    std::atomic<SLuint32> state{SL_OBJECT_STATE_UNREALIZED};
    std::atomic<SLuint32> recordState{SL_RECORDSTATE_STOPPED};

    cordial::audio::CaptureStream capture;

    uint32_t rateHz = 0;
    uint32_t channels = 0;
    uint32_t bytesPerFrame = 0;
    uint32_t numBuffers = 2;

    std::mutex mutex;
    std::deque<RecordBuffer> pending;
    uint32_t enqueuedIndex = 0;
    slAndroidSimpleBufferQueueCallback callback = nullptr;
    void* callbackContext = nullptr;
    uint64_t framesRecorded = 0;

    std::thread pump;
    /// The pump's own id, kept separately from `pump` because `pump.detach()`
    /// resets `pump.get_id()` to "not a thread" — so asking `pump` whether the
    /// caller is it stops working precisely after the detach that
    /// `stop_capture` performs, which is exactly when `Destroy` needs the
    /// answer.
    std::atomic<std::thread::id> pumpId{};
    std::atomic<bool> pumping{false};
    /// Set by `Destroy` when it was called from the pump thread itself and so
    /// cannot free this object; the pump deletes itself on the way out
    /// instead. See `stop_capture` for why that case is not hypothetical.
    std::atomic<bool> abandoned{false};

    std::atomic<SLmillisecond> markerPositionMs{0};
    std::atomic<SLmillisecond> positionUpdatePeriodMs{0};

    /// Moves captured samples from `capture` into whatever buffer the caller
    /// has at the front of the queue, and fires the drain callback for each
    /// buffer that fills.
    ///
    /// A thread rather than a PipeWire callback because `CaptureStream` was
    /// built for `android.media.AudioRecord.read`, whose caller polls on its
    /// own schedule; the buffering therefore lives inside `CaptureStream` and
    /// what is left out here is a copy, not a realtime deadline. 2 ms is a
    /// quarter of the shortest buffer either caller uses (WebRTC's 10 ms
    /// frame), which is often enough that a buffer is never late for want of
    /// asking and rare enough to cost nothing measurable.
    void run_pump() {
        // Recorded by the pump itself, first thing, so that a callback firing
        // immediately still finds the answer set. Doing it from the thread that
        // started the pump would leave a window in which it was not.
        pumpId.store(std::this_thread::get_id());
        while (pumping.load()) {
            bool completed = false;
            slAndroidSimpleBufferQueueCallback cb = nullptr;
            void* ctx = nullptr;
            {
                std::lock_guard<std::mutex> lock(mutex);
                cb = callback;
                ctx = callbackContext;
                if (!pending.empty()) {
                    RecordBuffer& front = pending.front();
                    uint32_t got = capture.read(front.data + front.filled, front.size - front.filled);
                    front.filled += got;
                    if (bytesPerFrame != 0) framesRecorded += got / bytesPerFrame;
                    if (front.filled >= front.size) {
                        pending.pop_front();
                        completed = true;
                    }
                }
            }
            // Outside the lock, for the same reason the playback side does it:
            // the ordinary buffer-queue pattern is to re-enqueue from inside
            // this callback, which would deadlock against its own mutex.
            if (completed && cb) {
                cb(reinterpret_cast<SLAndroidSimpleBufferQueueItf>(&bufferQueueVtable), ctx);
            } else {
                std::this_thread::sleep_for(std::chrono::milliseconds(2));
            }
        }
        // Must be the last statement: `Destroy` hands ownership over this way
        // when it could not free the object itself. Nothing may touch a member
        // after it.
        if (abandoned.load()) delete this;
    }

    bool on_pump_thread() const { return pumpId.load() == std::this_thread::get_id(); }

    /// Opens the microphone and starts the pump. Called only from
    /// `record_SetRecordState(SL_RECORDSTATE_RECORDING)`.
    bool start_capture() {
        if (capture.is_open()) return true;
        // Empty target: `CaptureStream::open` connects to whatever PipeWire
        // calls the default source, read at this moment rather than cached, so
        // changing the desktop's default microphone between two recordings is
        // picked up without restarting the client.
        if (!capture.open(rateHz, channels, std::string())) {
            return false;
        }
        pumping.store(true);
        pump = std::thread([this] { run_pump(); });
        return true;
    }

    /// Stops the pump and destroys the capture stream. Idempotent, because
    /// `Destroy` after `SetRecordState(STOPPED)` is the ordinary sequence and
    /// a double close must not be an error.
    ///
    /// The `on_pump_thread` branch is not defensive padding. Stopping a
    /// recorder from inside its own buffer-queue callback is a normal thing for
    /// an OpenSL caller to do — it is how you stop after N buffers — and it
    /// arrives here on the pump thread, where `join()` is a thread joining
    /// itself, which is `std::terminate` rather than a deadlock you could at
    /// least see in a backtrace. The thread has already been told to stop and
    /// will return the moment the callback does, so detaching costs nothing;
    /// what it costs `Destroy` is dealt with by `abandoned`.
    void stop_capture() {
        pumping.store(false);
        if (pump.joinable()) {
            if (on_pump_thread()) {
                pump.detach();
            } else {
                pump.join();
            }
        }
        capture.close();
    }
};

SLresult recorder_Realize(SLObjectItf self, SLboolean) {
    // Opens nothing; see the section comment. The engine gets a realized
    // recorder and the microphone stays shut until it asks to record.
    strip_const<AudioRecorderObject>(self)->state.store(SL_OBJECT_STATE_REALIZED);
    return SL_RESULT_SUCCESS;
}
SLresult recorder_GetState(SLObjectItf self, SLuint32* pState) {
    if (!pState) return SL_RESULT_PARAMETER_INVALID;
    *pState = strip_const<AudioRecorderObject>(self)->state.load();
    return SL_RESULT_SUCCESS;
}
SLresult recorder_GetInterface(SLObjectItf self, SLInterfaceID iid, void* pInterface) {
    if (!pInterface) return SL_RESULT_PARAMETER_INVALID;
    auto* r = strip_const<AudioRecorderObject>(self);
    if (iid == SL_IID_RECORD) {
        *reinterpret_cast<const void**>(pInterface) = &r->recordVtable;
        return SL_RESULT_SUCCESS;
    }
    if (iid == SL_IID_ANDROIDSIMPLEBUFFERQUEUE) {
        *reinterpret_cast<const void**>(pInterface) = &r->bufferQueueVtable;
        return SL_RESULT_SUCCESS;
    }
    if (iid == SL_IID_ANDROIDCONFIGURATION) {
        *reinterpret_cast<const void**>(pInterface) = &r->androidConfigVtable;
        return SL_RESULT_SUCCESS;
    }
    return SL_RESULT_FEATURE_UNSUPPORTED;
}
void recorder_Destroy(SLObjectItf self) {
    auto* r = strip_const<AudioRecorderObject>(self);
    // Before anything else: a destroyed recorder must not leave a microphone
    // behind it, including when the engine destroys one it never stopped. This
    // is the line that makes the privacy rule hold for a caller that simply
    // drops the object.
    r->stop_capture();
    r->recordState.store(SL_RECORDSTATE_STOPPED);
    r->state.store(SL_OBJECT_STATE_UNREALIZED);
    if (r->on_pump_thread()) {
        // Destroyed from inside its own buffer callback. The pump is still on
        // the stack below us, so freeing here would return into a deleted
        // object; `run_pump` frees itself instead once it unwinds. The
        // microphone is already shut either way, which is the part that could
        // not have waited.
        r->abandoned.store(true);
        return;
    }
    delete r;
}

AudioRecorderObject* recorder_from_record(SLRecordItf self) {
    return reinterpret_cast<AudioRecorderObject*>(strip_const<char>(self) -
                                                   offsetof(AudioRecorderObject, recordVtable));
}

SLresult record_SetRecordState(SLRecordItf self, SLuint32 state) {
    auto* r = recorder_from_record(self);
    switch (state) {
        case SL_RECORDSTATE_RECORDING:
            if (!r->start_capture()) {
                // Stays stopped. A recorder reporting RECORDING with no stream
                // behind it is the stub-that-lies pattern this project keeps
                // paying for: the engine would sit on a buffer queue that never
                // drains, with nothing to say why.
                std::fprintf(stderr,
                    "E/Cordial-OpenSLES         SetRecordState(RECORDING) could not open a "
                    "capture stream; staying STOPPED rather than reporting a recording that "
                    "is not happening.\n");
                r->recordState.store(SL_RECORDSTATE_STOPPED);
                return SL_RESULT_RESOURCE_ERROR;
            }
            break;
        case SL_RECORDSTATE_PAUSED:
        case SL_RECORDSTATE_STOPPED:
            // Both destroy the stream. See the section comment on why pausing
            // is not allowed to mean "keep the device, discard the samples".
            r->stop_capture();
            break;
        default:
            return SL_RESULT_PARAMETER_INVALID;
    }
    r->recordState.store(state);
    return SL_RESULT_SUCCESS;
}
SLresult record_GetRecordState(SLRecordItf self, SLuint32* pState) {
    if (!pState) return SL_RESULT_PARAMETER_INVALID;
    *pState = recorder_from_record(self)->recordState.load();
    return SL_RESULT_SUCCESS;
}
SLresult record_SetDurationLimit(SLRecordItf, SLmillisecond) {
    // A promise to stop recording after N milliseconds. Nothing here enforces
    // one, and accepting it would be a promise about the microphone
    // specifically — the worst possible subject for a stub to be wrong about.
    return SL_RESULT_FEATURE_UNSUPPORTED;
}
SLresult record_GetPosition(SLRecordItf self, SLmillisecond* pMsec) {
    if (!pMsec) return SL_RESULT_PARAMETER_INVALID;
    auto* r = recorder_from_record(self);
    std::lock_guard<std::mutex> lock(r->mutex);
    // Genuinely tracked, unlike the playback side's `play_GetPosition`: the
    // pump counts every frame it hands to the caller, so this is a real
    // position rather than an estimate that would have to be invented.
    *pMsec = r->rateHz == 0 ? 0
                            : static_cast<SLmillisecond>(r->framesRecorded * 1000 / r->rateHz);
    return SL_RESULT_SUCCESS;
}
SLresult record_RegisterCallback(SLRecordItf, void*, void*) {
    return SL_RESULT_SUCCESS; // accepted; see SetCallbackEventsMask
}
SLresult record_SetCallbackEventsMask(SLRecordItf, SLuint32) {
    // SL_RECORDEVENT_HEADATLIMIT and friends all need the duration limit and
    // marker machinery this recorder does not run. Accepting interest in an
    // event that structurally never occurs is not the same claim as promising
    // it will fire — the same reasoning as `play_SetCallbackEventsMask`.
    return SL_RESULT_SUCCESS;
}
SLresult record_GetCallbackEventsMask(SLRecordItf, SLuint32* pEventFlags) {
    if (pEventFlags) *pEventFlags = 0;
    return SL_RESULT_SUCCESS;
}
SLresult record_SetMarkerPosition(SLRecordItf self, SLmillisecond mSec) {
    recorder_from_record(self)->markerPositionMs.store(mSec);
    return SL_RESULT_SUCCESS;
}
SLresult record_ClearMarkerPosition(SLRecordItf self) {
    recorder_from_record(self)->markerPositionMs.store(0);
    return SL_RESULT_SUCCESS;
}
SLresult record_GetMarkerPosition(SLRecordItf self, SLmillisecond* pMsec) {
    if (pMsec) *pMsec = recorder_from_record(self)->markerPositionMs.load();
    return SL_RESULT_SUCCESS;
}
SLresult record_SetPositionUpdatePeriod(SLRecordItf self, SLmillisecond mSec) {
    recorder_from_record(self)->positionUpdatePeriodMs.store(mSec);
    return SL_RESULT_SUCCESS;
}
SLresult record_GetPositionUpdatePeriod(SLRecordItf self, SLmillisecond* pMsec) {
    if (pMsec) *pMsec = recorder_from_record(self)->positionUpdatePeriodMs.load();
    return SL_RESULT_SUCCESS;
}

AudioRecorderObject* recorder_from_absq(SLAndroidSimpleBufferQueueItf self) {
    return reinterpret_cast<AudioRecorderObject*>(strip_const<char>(self) -
                                                   offsetof(AudioRecorderObject, bufferQueueVtable));
}

SLresult rec_absq_Enqueue(SLAndroidSimpleBufferQueueItf self, const void* pBuffer, SLuint32 size) {
    if (!pBuffer || size == 0) return SL_RESULT_PARAMETER_INVALID;
    auto* r = recorder_from_absq(self);
    std::lock_guard<std::mutex> lock(r->mutex);
    if (r->pending.size() >= r->numBuffers) return SL_RESULT_BUFFER_INSUFFICIENT;
    // On the recording side the caller's buffer is written to rather than read
    // from, so the const has to come off. It is the caller's own memory and the
    // buffer-queue contract is precisely that it is ours to fill until the
    // drain callback says otherwise.
    r->pending.push_back({static_cast<uint8_t*>(const_cast<void*>(pBuffer)), size, 0});
    ++r->enqueuedIndex;
    return SL_RESULT_SUCCESS;
}
SLresult rec_absq_Clear(SLAndroidSimpleBufferQueueItf self) {
    auto* r = recorder_from_absq(self);
    std::lock_guard<std::mutex> lock(r->mutex);
    r->pending.clear();
    return SL_RESULT_SUCCESS;
}
SLresult rec_absq_GetState(SLAndroidSimpleBufferQueueItf self,
                            SLAndroidSimpleBufferQueueState* pState) {
    if (!pState) return SL_RESULT_PARAMETER_INVALID;
    auto* r = recorder_from_absq(self);
    std::lock_guard<std::mutex> lock(r->mutex);
    pState->count = static_cast<SLuint32>(r->pending.size());
    pState->index = r->enqueuedIndex;
    return SL_RESULT_SUCCESS;
}
SLresult rec_absq_RegisterCallback(SLAndroidSimpleBufferQueueItf self,
                                    slAndroidSimpleBufferQueueCallback callback, void* pContext) {
    auto* r = recorder_from_absq(self);
    std::lock_guard<std::mutex> lock(r->mutex);
    r->callback = callback;
    r->callbackContext = pContext;
    return SL_RESULT_SUCCESS;
}

// ------------------------------------------------------------------- vtables

constexpr SLObjectItf_ kEngineObjectMethods = {
    engine_Realize, object_Resume, engine_GetState, engine_GetInterface, object_RegisterCallback,
    object_AbortAsyncOperation, engine_Destroy, object_SetPriority, object_GetPriority,
    object_SetLossOfControlInterfaces,
};

constexpr SLObjectItf_ kOutputMixObjectMethods = {
    outputmixobj_Realize, object_Resume, outputmixobj_GetState, outputmixobj_GetInterface,
    object_RegisterCallback, object_AbortAsyncOperation, outputmixobj_Destroy, object_SetPriority,
    object_GetPriority, object_SetLossOfControlInterfaces,
};

constexpr SLObjectItf_ kAudioPlayerObjectMethods = {
    player_Realize, object_Resume, player_GetState, player_GetInterface, object_RegisterCallback,
    object_AbortAsyncOperation, player_Destroy, object_SetPriority, object_GetPriority,
    object_SetLossOfControlInterfaces,
};

constexpr SLObjectItf_ kAudioRecorderObjectMethods = {
    recorder_Realize, object_Resume, recorder_GetState, recorder_GetInterface,
    object_RegisterCallback, object_AbortAsyncOperation, recorder_Destroy, object_SetPriority,
    object_GetPriority, object_SetLossOfControlInterfaces,
};

constexpr SLRecordItf_ kRecordMethods = {
    record_SetRecordState, record_GetRecordState, record_SetDurationLimit, record_GetPosition,
    record_RegisterCallback, record_SetCallbackEventsMask, record_GetCallbackEventsMask,
    record_SetMarkerPosition, record_ClearMarkerPosition, record_GetMarkerPosition,
    record_SetPositionUpdatePeriod, record_GetPositionUpdatePeriod,
};

/// A separate table from the player's `kAndroidSimpleBufferQueueMethods`
/// because the two queues run in opposite directions — the player's `Enqueue`
/// hands over full buffers, the recorder's hands over empty ones — and sharing
/// one table would mean a run-time type tag deciding which meaning applied.
constexpr SLAndroidSimpleBufferQueueItf_ kRecorderBufferQueueMethods = {
    rec_absq_Enqueue, rec_absq_Clear, rec_absq_GetState, rec_absq_RegisterCallback,
};

constexpr SLOutputMixItf_ kOutputMixMethods = {
    outputmix_GetDestinationOutputDeviceIDs,
    outputmix_RegisterDeviceChangeCallback,
    outputmix_ReRoute,
};

constexpr SLPlayItf_ kPlayMethods = {
    play_SetPlayState, play_GetPlayState, play_GetDuration, play_GetPosition, play_RegisterCallback,
    play_SetCallbackEventsMask, play_GetCallbackEventsMask, play_SetMarkerPosition,
    play_ClearMarkerPosition, play_GetMarkerPosition, play_SetPositionUpdatePeriod,
    play_GetPositionUpdatePeriod,
};

constexpr SLAndroidSimpleBufferQueueItf_ kAndroidSimpleBufferQueueMethods = {
    absq_Enqueue, absq_Clear, absq_GetState, absq_RegisterCallback,
};

constexpr SLVolumeItf_ kVolumeMethods = {
    volume_SetVolumeLevel, volume_GetVolumeLevel, volume_GetMaxVolumeLevel, volume_SetMute,
    volume_GetMute, volume_EnableStereoPosition, volume_IsEnabledStereoPosition,
    volume_SetStereoPosition, volume_GetStereoPosition,
};

constexpr SLAndroidConfigurationItf_ kAndroidConfigurationMethods = {
    androidconfig_SetConfiguration, androidconfig_GetConfiguration, androidconfig_AcquireJavaProxy,
    androidconfig_ReleaseJavaProxy,
};

// ------------------------------------------------------------------- SLEngineItf

SLresult engine_CreateLEDDevice(SLEngineItf, SLObjectItf* pDevice, SLuint32, SLuint32,
                                 const SLInterfaceID*, const SLboolean*) {
    if (pDevice) *pDevice = nullptr;
    return SL_RESULT_FEATURE_UNSUPPORTED;
}
SLresult engine_CreateVibraDevice(SLEngineItf, SLObjectItf* pDevice, SLuint32, SLuint32,
                                   const SLInterfaceID*, const SLboolean*) {
    if (pDevice) *pDevice = nullptr;
    return SL_RESULT_FEATURE_UNSUPPORTED;
}

SLresult engine_CreateAudioPlayer(SLEngineItf, SLObjectItf* pPlayer, SLDataSource* pAudioSrc,
                                   SLDataSink* pAudioSnk, SLuint32 numInterfaces,
                                   const SLInterfaceID* pInterfaceIds,
                                   const SLboolean* pInterfaceRequired) {
    if (!pPlayer) return SL_RESULT_PARAMETER_INVALID;
    *pPlayer = nullptr;
    if (!pAudioSrc || !pAudioSnk || !pAudioSrc->pLocator || !pAudioSrc->pFormat || !pAudioSnk->pLocator) {
        return SL_RESULT_PARAMETER_INVALID;
    }

    SLuint32 srcLocatorType = *static_cast<const SLuint32*>(pAudioSrc->pLocator);
    if (srcLocatorType != SL_DATALOCATOR_ANDROIDSIMPLEBUFFERQUEUE) {
        // A real OpenSL ES source type (URI, ANDROIDFD, plain BUFFERQUEUE,
        // ...) Roblox is entitled to ask for; this backend only implements
        // the raw-PCM push model Android's own audio players actually use.
        return SL_RESULT_CONTENT_UNSUPPORTED;
    }
    auto* srcLocator = static_cast<const SLDataLocator_AndroidSimpleBufferQueue*>(pAudioSrc->pLocator);

    SLuint32 formatType = *static_cast<const SLuint32*>(pAudioSrc->pFormat);
    SLuint32 numChannels, samplesPerSecMilli, bitsPerSample, containerSize, endianness;
    if (formatType == SL_DATAFORMAT_PCM) {
        auto* fmt = static_cast<const SLDataFormat_PCM*>(pAudioSrc->pFormat);
        numChannels = fmt->numChannels;
        samplesPerSecMilli = fmt->samplesPerSec;
        bitsPerSample = fmt->bitsPerSample;
        containerSize = fmt->containerSize;
        endianness = fmt->endianness;
    } else if (formatType == SL_ANDROID_DATAFORMAT_PCM_EX) {
        auto* fmt = static_cast<const SLAndroidDataFormat_PCM_EX*>(pAudioSrc->pFormat);
        if (fmt->representation != SL_ANDROID_PCM_REPRESENTATION_SIGNED_INT) {
            // Float and unsigned-int PCM: not translated. Rejecting here is
            // the honest answer; silently reinterpreting the bytes as signed
            // would produce audio, just wrong audio.
            return SL_RESULT_CONTENT_UNSUPPORTED;
        }
        numChannels = fmt->numChannels;
        samplesPerSecMilli = fmt->sampleRate;
        bitsPerSample = fmt->bitsPerSample;
        containerSize = fmt->containerSize;
        endianness = fmt->endianness;
    } else {
        return SL_RESULT_CONTENT_UNSUPPORTED; // MIME/compressed sources: no decoder here
    }

    SLuint32 snkLocatorType = *static_cast<const SLuint32*>(pAudioSnk->pLocator);
    if (snkLocatorType != SL_DATALOCATOR_OUTPUTMIX) {
        return SL_RESULT_CONTENT_UNSUPPORTED;
    }
    // The OutputMix object itself is not otherwise consulted: this backend
    // has exactly one destination (the shared PipeWire session), so there is
    // nothing routing-relevant to read from it. A null one is still a
    // caller bug worth reporting rather than silently playing anyway.
    auto* snkLocator = static_cast<const SLDataLocator_OutputMix*>(pAudioSnk->pLocator);
    if (!snkLocator->outputMix) return SL_RESULT_PARAMETER_INVALID;

    for (SLuint32 i = 0; i < numInterfaces; ++i) {
        SLInterfaceID id = pInterfaceIds[i];
        bool required = pInterfaceRequired ? pInterfaceRequired[i] != SL_BOOLEAN_FALSE : true;
        bool known = id == SL_IID_PLAY || id == SL_IID_ANDROIDSIMPLEBUFFERQUEUE ||
                     id == SL_IID_VOLUME || id == SL_IID_ANDROIDCONFIGURATION;
        if (required && !known) return SL_RESULT_FEATURE_UNSUPPORTED;
    }

    auto* player = new AudioPlayerObject();
    player->objectVtable = &kAudioPlayerObjectMethods;
    player->playVtable = &kPlayMethods;
    player->bufferQueueVtable = &kAndroidSimpleBufferQueueMethods;
    player->volumeVtable = &kVolumeMethods;
    player->androidConfigVtable = &kAndroidConfigurationMethods;
    player->rateHz = samplesPerSecMilli / 1000; // milliHertz -> Hz; 44100000 means 44.1kHz
    player->channels = numChannels;
    player->bitsPerSample = bitsPerSample;
    player->containerBits = containerSize;
    player->bigEndian = endianness == SL_BYTEORDER_BIGENDIAN;
    player->numBuffers = srcLocator->numBuffers != 0 ? srcLocator->numBuffers : 2;

    *pPlayer = reinterpret_cast<SLObjectItf>(&player->objectVtable);
    return SL_RESULT_SUCCESS;
}

SLresult engine_CreateAudioRecorder(SLEngineItf, SLObjectItf* pRecorder, SLDataSource* pAudioSrc,
                                     SLDataSink* pAudioSnk, SLuint32 numInterfaces,
                                     const SLInterfaceID* pInterfaceIds,
                                     const SLboolean* pInterfaceRequired) {
    if (!pRecorder) return SL_RESULT_PARAMETER_INVALID;
    *pRecorder = nullptr;
    if (!pAudioSrc || !pAudioSnk || !pAudioSrc->pLocator || !pAudioSnk->pLocator ||
        !pAudioSnk->pFormat) {
        return SL_RESULT_PARAMETER_INVALID;
    }

    // Source: an IO device, and specifically the default audio input. A
    // request for a *named* input device is refused rather than quietly served
    // from the default — the caller asked for a particular microphone and
    // getting a different one without being told is worse than being refused.
    // (Which microphone the default is comes from PipeWire; Android's device
    // list, including how a caller would learn the ids, is `audio_classes.cpp`.)
    SLuint32 srcLocatorType = *static_cast<const SLuint32*>(pAudioSrc->pLocator);
    if (srcLocatorType != SL_DATALOCATOR_IODEVICE) return SL_RESULT_CONTENT_UNSUPPORTED;
    auto* srcLocator = static_cast<const SLDataLocator_IODevice*>(pAudioSrc->pLocator);
    if (srcLocator->deviceType != SL_IODEVICE_AUDIOINPUT) return SL_RESULT_CONTENT_UNSUPPORTED;
    if (srcLocator->deviceID != SL_DEFAULTDEVICEID_AUDIOINPUT) {
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         CreateAudioRecorder asked for input device id 0x%08X; "
            "only SL_DEFAULTDEVICEID_AUDIOINPUT is implemented, and serving a different "
            "microphone than the one asked for would be worse than refusing.\n",
            srcLocator->deviceID);
        return SL_RESULT_CONTENT_UNSUPPORTED;
    }

    SLuint32 snkLocatorType = *static_cast<const SLuint32*>(pAudioSnk->pLocator);
    if (snkLocatorType != SL_DATALOCATOR_ANDROIDSIMPLEBUFFERQUEUE) {
        return SL_RESULT_CONTENT_UNSUPPORTED;
    }
    auto* snkLocator = static_cast<const SLDataLocator_AndroidSimpleBufferQueue*>(pAudioSnk->pLocator);

    SLuint32 formatType = *static_cast<const SLuint32*>(pAudioSnk->pFormat);
    if (formatType != SL_DATAFORMAT_PCM) return SL_RESULT_CONTENT_UNSUPPORTED;
    auto* fmt = static_cast<const SLDataFormat_PCM*>(pAudioSnk->pFormat);
    // `CaptureStream` negotiates SPA_AUDIO_FORMAT_S16 and only that, so
    // anything else is refused here rather than delivered as reinterpreted
    // bytes. Playback can afford a lookup table of layouts because PipeWire
    // converts on the way out; capture would have to convert on the way in,
    // which is work nothing has asked for.
    if (fmt->bitsPerSample != 16 || fmt->containerSize != 16 ||
        fmt->endianness == SL_BYTEORDER_BIGENDIAN) {
        std::fprintf(stderr,
            "E/Cordial-OpenSLES         CreateAudioRecorder asked for %u-bit samples in a "
            "%u-bit container; this backend records signed 16-bit native-endian PCM only.\n",
            fmt->bitsPerSample, fmt->containerSize);
        return SL_RESULT_CONTENT_UNSUPPORTED;
    }
    if (fmt->numChannels == 0 || fmt->samplesPerSec == 0) return SL_RESULT_PARAMETER_INVALID;

    for (SLuint32 i = 0; i < numInterfaces; ++i) {
        SLInterfaceID id = pInterfaceIds[i];
        bool required = pInterfaceRequired ? pInterfaceRequired[i] != SL_BOOLEAN_FALSE : true;
        bool known = id == SL_IID_RECORD || id == SL_IID_ANDROIDSIMPLEBUFFERQUEUE ||
                     id == SL_IID_ANDROIDCONFIGURATION;
        if (required && !known) return SL_RESULT_FEATURE_UNSUPPORTED;
    }

    auto* recorder = new AudioRecorderObject();
    recorder->objectVtable = &kAudioRecorderObjectMethods;
    recorder->recordVtable = &kRecordMethods;
    recorder->bufferQueueVtable = &kRecorderBufferQueueMethods;
    recorder->androidConfigVtable = &kAndroidConfigurationMethods;
    recorder->rateHz = fmt->samplesPerSec / 1000; // milliHertz -> Hz, as for playback
    recorder->channels = fmt->numChannels;
    recorder->bytesPerFrame = fmt->numChannels * 2; // S16, checked above
    recorder->numBuffers = snkLocator->numBuffers != 0 ? snkLocator->numBuffers : 2;

    // Worth a line even on the success path: this is the moment a Roblox that
    // records through OpenSL ES becomes distinguishable from one that does not,
    // and nobody has yet observed it happen. No microphone is opened here.
    std::fprintf(stderr,
        "I/Cordial-OpenSLES         CreateAudioRecorder(%u Hz, %u channel(s), %u buffers) — "
        "recorder created; no capture stream exists until SetRecordState(RECORDING).\n",
        recorder->rateHz, recorder->channels, recorder->numBuffers);

    *pRecorder = reinterpret_cast<SLObjectItf>(&recorder->objectVtable);
    return SL_RESULT_SUCCESS;
}
SLresult engine_CreateMidiPlayer(SLEngineItf, SLObjectItf* pPlayer, SLDataSource*, SLDataSource*,
                                  SLDataSink*, SLDataSink*, SLDataSink*, SLuint32, const SLInterfaceID*,
                                  const SLboolean*) {
    if (pPlayer) *pPlayer = nullptr;
    return SL_RESULT_FEATURE_UNSUPPORTED;
}
SLresult engine_CreateListener(SLEngineItf, SLObjectItf* pListener, SLuint32, const SLInterfaceID*,
                                const SLboolean*) {
    if (pListener) *pListener = nullptr;
    return SL_RESULT_FEATURE_UNSUPPORTED;
}
SLresult engine_Create3DGroup(SLEngineItf, SLObjectItf* pGroup, SLuint32, const SLInterfaceID*,
                               const SLboolean*) {
    if (pGroup) *pGroup = nullptr;
    return SL_RESULT_FEATURE_UNSUPPORTED;
}

SLresult engine_CreateOutputMix(SLEngineItf, SLObjectItf* pMix, SLuint32 numInterfaces,
                                 const SLInterfaceID* pInterfaceIds,
                                 const SLboolean* pInterfaceRequired) {
    if (!pMix) return SL_RESULT_PARAMETER_INVALID;
    *pMix = nullptr;
    for (SLuint32 i = 0; i < numInterfaces; ++i) {
        bool required = pInterfaceRequired ? pInterfaceRequired[i] != SL_BOOLEAN_FALSE : true;
        if (required && pInterfaceIds[i] != &g_output_mix) return SL_RESULT_FEATURE_UNSUPPORTED;
    }
    auto* mix = new OutputMixObject();
    mix->objectVtable = &kOutputMixObjectMethods;
    mix->outputMixVtable = &kOutputMixMethods;
    *pMix = reinterpret_cast<SLObjectItf>(&mix->objectVtable);
    return SL_RESULT_SUCCESS;
}

SLresult engine_CreateMetadataExtractor(SLEngineItf, SLObjectItf* pMetadataExtractor, SLDataSource*,
                                         SLuint32, const SLInterfaceID*, const SLboolean*) {
    if (pMetadataExtractor) *pMetadataExtractor = nullptr;
    return SL_RESULT_FEATURE_UNSUPPORTED;
}
SLresult engine_CreateExtensionObject(SLEngineItf, SLObjectItf* pObject, void*, SLuint32, SLuint32,
                                       const SLInterfaceID*, const SLboolean*) {
    if (pObject) *pObject = nullptr;
    return SL_RESULT_FEATURE_UNSUPPORTED;
}
SLresult engine_QueryNumSupportedInterfaces(SLEngineItf, SLuint32, SLuint32* pNumSupportedInterfaces) {
    if (pNumSupportedInterfaces) *pNumSupportedInterfaces = 0;
    return SL_RESULT_SUCCESS;
}
SLresult engine_QuerySupportedInterfaces(SLEngineItf, SLuint32, SLuint32, SLInterfaceID*) {
    return SL_RESULT_PARAMETER_INVALID; // index out of range: QueryNum* above always reports zero
}
SLresult engine_QueryNumSupportedExtensions(SLEngineItf, SLuint32* pNumExtensions) {
    if (pNumExtensions) *pNumExtensions = 0;
    return SL_RESULT_SUCCESS;
}
SLresult engine_QuerySupportedExtension(SLEngineItf, SLuint32, SLchar*, SLint16* pNameLength) {
    if (pNameLength) *pNameLength = 0;
    return SL_RESULT_PARAMETER_INVALID;
}
SLresult engine_IsExtensionSupported(SLEngineItf, const SLchar*, SLboolean* pSupported) {
    if (pSupported) *pSupported = SL_BOOLEAN_FALSE;
    return SL_RESULT_SUCCESS;
}

constexpr SLEngineItf_ kEngineMethods = {
    engine_CreateLEDDevice, engine_CreateVibraDevice, engine_CreateAudioPlayer,
    engine_CreateAudioRecorder, engine_CreateMidiPlayer, engine_CreateListener, engine_Create3DGroup,
    engine_CreateOutputMix, engine_CreateMetadataExtractor, engine_CreateExtensionObject,
    engine_QueryNumSupportedInterfaces, engine_QuerySupportedInterfaces,
    engine_QueryNumSupportedExtensions, engine_QuerySupportedExtension, engine_IsExtensionSupported,
};

} // namespace

extern "C" {

/// `SL_RESULT_FEATURE_UNSUPPORTED` when PipeWire is unreachable — no
/// library, no session, or no `pipewire-devel` at build time (see
/// `pipewire_backend.cpp`, which prints the specific reason). Handing back
/// an engine object in that case would let Roblox proceed on an engine with
/// no audio behind it and fail somewhere with no relationship to the real
/// cause; that is the mistake this file has avoided since it first existed,
/// now for a more specific reason than "nothing is implemented yet".
uint32_t slCreateEngine(void** engine, uint32_t numOptions, const void* pEngineOptions,
                         uint32_t numInterfaces, const void* pInterfaceIds,
                         const void* pInterfaceRequired) {
    // SL_ENGINEOPTION_THREADSAFE / _LOSSOFCONTROL: this engine is internally
    // synchronised unconditionally (every mutable object above is behind an
    // atomic or a mutex), so there is no laxer mode for these to opt out of.
    (void)numOptions;
    (void)pEngineOptions;

    // Announced on entry, before anything can decline. Whether the engine calls
    // this at all is the first question about Cordial's audio and it has been
    // answered "not at the Landing screen" only — with no line printed on the
    // way in, an absent engine and a refused one looked identical in a log, and
    // the PipeWire "session reachable" line below is not a substitute because
    // it is also printed by device enumeration and by `supportsAAudio()` --
    // which is why that line no longer claims anything about OpenSL ES.
    std::fprintf(stderr,
        "I/Cordial-OpenSLES         slCreateEngine called by the engine (%u option(s), %u "
        "interface(s) requested).\n", numOptions, numInterfaces);

    if (engine) *engine = nullptr;

    for (uint32_t i = 0; i < numInterfaces; ++i) {
        SLInterfaceID id = static_cast<const SLInterfaceID*>(pInterfaceIds)[i];
        bool required = pInterfaceRequired ? static_cast<const SLboolean*>(pInterfaceRequired)[i] !=
                                                  SL_BOOLEAN_FALSE
                                            : true;
        if (required && id != SL_IID_ENGINE) return SL_RESULT_FEATURE_UNSUPPORTED;
    }

    if (!cordial::audio::pipewire_available()) {
        return SL_RESULT_FEATURE_UNSUPPORTED;
    }
    if (!engine) return SL_RESULT_PARAMETER_INVALID;

    auto* e = new EngineObject();
    e->objectVtable = &kEngineObjectMethods;
    e->engineVtable = &kEngineMethods;
    *engine = const_cast<void*>(static_cast<const void*>(&e->objectVtable));
    return SL_RESULT_SUCCESS;
}

struct CordialSymbol {
    const char* name;
    void* address;
};

static const CordialSymbol kSymbols[] = {
    {"slCreateEngine", reinterpret_cast<void*>(&slCreateEngine)},
    {"SL_IID_ENGINE", &SL_IID_ENGINE},
    {"SL_IID_PLAY", &SL_IID_PLAY},
    {"SL_IID_RECORD", &SL_IID_RECORD},
    {"SL_IID_VOLUME", &SL_IID_VOLUME},
    {"SL_IID_BUFFERQUEUE", &SL_IID_BUFFERQUEUE},
    {"SL_IID_ANDROIDSIMPLEBUFFERQUEUE", &SL_IID_ANDROIDSIMPLEBUFFERQUEUE},
    {"SL_IID_ANDROIDCONFIGURATION", &SL_IID_ANDROIDCONFIGURATION},
};

const CordialSymbol* cordial_opensles_symbols(size_t* count) {
    if (count) *count = sizeof(kSymbols) / sizeof(kSymbols[0]);
    return kSymbols;
}

} // extern "C"
