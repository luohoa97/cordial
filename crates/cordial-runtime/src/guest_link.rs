//! What the arm64 guest's imports resolve to (docs/vr/dynarmic-design.md §2,
//! §3.1): the table handed to the bionic linker when the Quest build's
//! `libroblox.so` is linked into an x86-64 process for the translator to run.
//!
//! **Every function import resolves to a stub in `cordial-guest`'s stub page,
//! never to a host function pointer.** The guest runs arm64 instructions; a
//! host address handed to it would be jumped to as arm64 code by the
//! translator, or, worse, called as x86 code by something that trusted the
//! table. The linker enforces the same line from its side (`patches/0006`:
//! a guest object binds only to libraries marked guest), so a symbol missing
//! here fails the load by name rather than finding a host definition.
//!
//! What sits behind each stub is one of:
//!
//! * **a Cordial or host implementation**, reached through the generic
//!   AAPCS64-to-SysV call builder, for a function whose signature is written
//!   down in `FUNCS` below and whose arguments mean the same thing on both
//!   sides. The implementation is whatever `symtab::build` picks for the
//!   native x86-64 path, so the guest and the native engine get the same
//!   answer. The table is deliberately a vetted list and not "everything
//!   Cordial implements": `stat`, `sigaction`, `epoll_event` and the `O_*`
//!   flags are laid out or numbered differently on arm64 (design §3.1), and a
//!   Cordial implementation written for the x86-64 phone build would be
//!   silently wrong there.
//! * **a hand-written thunk**: from `cordial_guest::thunks` (printf-family
//!   calls, `qsort`, `pthread_create`), or from `guest_libc`, for the calls
//!   the M3 constructors and `JNI_OnLoad` reached that need more than a
//!   register move -- the CPU's hwcaps, guest callbacks, the linker's own
//!   `dl*`, bionic's locale model, `va_list`, scanf, arm64's `struct stat`
//!   and `O_*` numbers, syscall numbering, and executable mappings.
//! * **the host's OpenXR loader**, for the guest's `libopenxr_loader.so`,
//!   which is never loaded itself (§2): `guest_xr` bridges each command to
//!   `libopenxr_loader.so.1` and on to the runtime it selects (M6).
//! * **Cordial's AAudio**, for the guest's `libaaudio.so`, which the engine
//!   `dlopen`s rather than imports: `guest_audio` forwards each entry point
//!   and hands the data and error callbacks to the host as host entries.
//! * **the Meta platform loader's honest failure**, for the guest's
//!   `libovrplatformloader.so`: `guest_ovr` answers each `ovr_*` name as a
//!   host with no Meta platform services, every request failing through the
//!   message queue.
//! * **a stop.** Everything else is registered as a stub that halts the guest with a
//!   `Fault` naming the function and saying what is missing. It never
//!   returns a made-up value. Calling one is where the next piece of work is.
//!
//! Data imports get host storage, which works because guest and host
//! addresses are the same (§2), and only where the layout is the one bionic
//! has on arm64. See `data_answer`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_char, c_void};
use std::sync::Arc;

use cordial_guest::{thunks, Fault, Handler, Ret, Runtime, Ty};

use crate::elf::{Binding, Imports};
use crate::symtab::{self, Source, SymbolTable};

#[path = "guest_gl_table.rs"]
mod gl_table;

pub const OPENXR_LOADER: &str = "libopenxr_loader.so";
pub const OVR_PLATFORM_LOADER: &str = "libovrplatformloader.so";

/// How one import was answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Answer {
    /// A stub dispatching to Cordial's own implementation.
    Cordial,
    /// A stub dispatching to the host's library (glibc, libm).
    Host,
    /// A stub running a hand-written thunk.
    Thunk,
    /// A stub in the virtual OpenXR loader, bridged to the host's loader
    /// (`guest_xr`), or failing as a loader with no runtime would if the host
    /// has none.
    OpenXr,
    /// A stub that stops the guest with a named fault when called.
    Stop,
    /// Host storage for a data import.
    Data,
    /// A weak import Android's own libraries do not define either, left for
    /// the linker to resolve to zero exactly as it would on the Quest.
    WeakNull,
    /// Nothing: a data import no storage exists for, or one left out on
    /// purpose with `CORDIAL_GUEST_OMIT`. A strong one fails the load.
    Unanswered,
}

impl Answer {
    pub fn label(self) -> &'static str {
        match self {
            Answer::Cordial => "cordial",
            Answer::Host => "host",
            Answer::Thunk => "thunk",
            Answer::OpenXr => "openxr",
            Answer::Stop => "stop",
            Answer::Data => "data",
            Answer::WeakNull => "weak-null",
            Answer::Unanswered => "UNANSWERED",
        }
    }
}

pub struct Row {
    pub symbol: String,
    pub library: String,
    pub answer: Answer,
    /// For a stop, why; for data, where the storage is.
    pub note: String,
}

pub struct GuestTable {
    /// soname -> (symbol, guest address), for `linker::register_guest`.
    pub libraries: BTreeMap<String, Vec<(String, *mut c_void)>>,
    /// One row per import, in name order.
    pub rows: Vec<Row>,
}

impl GuestTable {
    pub fn count(&self, a: Answer) -> usize {
        self.rows.iter().filter(|r| r.answer == a).count()
    }
}

use Ty::{F32, F64, I32, I64, Ptr, U32, U64};

/// Functions the generic call builder can reach, with their C signatures.
///
/// A name belongs here only when every argument is a scalar or a pointer to
/// memory laid out identically for arm64 bionic and for the implementation
/// that answers it: byte strings, `timespec`/`timeval`/`tm`, bionic's LP64
/// pthread types (the same on arm64 and x86-64, which is why the native
/// path's answer is the guest's too), and `FILE*` values the guest only ever
/// passes back. `CORDIAL_ONLY` narrows that to Cordial's own translation
/// where the host's constants differ from bionic's.
///
/// The return types are what the callee writes; narrow integers are extended
/// by the call builder either way.
#[rustfmt::skip]
const FUNCS: &[(&str, &[Ty], Ret)] = &[
    // libm. Scalars only, or out-pointers to plain floats and ints.
    ("acos", &[F64], Ret::F64), ("asin", &[F64], Ret::F64), ("atan", &[F64], Ret::F64),
    ("cbrt", &[F64], Ret::F64), ("cos", &[F64], Ret::F64), ("cosh", &[F64], Ret::F64),
    ("exp", &[F64], Ret::F64), ("exp2", &[F64], Ret::F64), ("expm1", &[F64], Ret::F64),
    ("log", &[F64], Ret::F64), ("log10", &[F64], Ret::F64), ("log2", &[F64], Ret::F64),
    ("round", &[F64], Ret::F64), ("sin", &[F64], Ret::F64), ("sinh", &[F64], Ret::F64),
    ("tan", &[F64], Ret::F64), ("tanh", &[F64], Ret::F64),
    ("acosf", &[F32], Ret::F32), ("asinf", &[F32], Ret::F32), ("atanf", &[F32], Ret::F32),
    ("cbrtf", &[F32], Ret::F32), ("cosf", &[F32], Ret::F32), ("coshf", &[F32], Ret::F32),
    ("erfcf", &[F32], Ret::F32), ("erff", &[F32], Ret::F32), ("exp2f", &[F32], Ret::F32),
    ("expf", &[F32], Ret::F32), ("log10f", &[F32], Ret::F32), ("log2f", &[F32], Ret::F32),
    ("logf", &[F32], Ret::F32), ("sinf", &[F32], Ret::F32), ("sinhf", &[F32], Ret::F32),
    ("tanf", &[F32], Ret::F32), ("tanhf", &[F32], Ret::F32),
    ("atan2", &[F64, F64], Ret::F64), ("fmod", &[F64, F64], Ret::F64), ("pow", &[F64, F64], Ret::F64),
    ("atan2f", &[F32, F32], Ret::F32), ("fmodf", &[F32, F32], Ret::F32),
    ("hypotf", &[F32, F32], Ret::F32), ("powf", &[F32, F32], Ret::F32),
    ("remainderf", &[F32, F32], Ret::F32), ("nextafterf", &[F32, F32], Ret::F32),
    ("ldexp", &[F64, I32], Ret::F64), ("ldexpf", &[F32, I32], Ret::F32),
    ("ilogb", &[F64], Ret::Int(I32)), ("finitef", &[F32], Ret::Int(I32)),
    ("frexp", &[F64, Ptr], Ret::F64), ("frexpf", &[F32, Ptr], Ret::F32),
    ("modf", &[F64, Ptr], Ret::F64), ("modff", &[F32, Ptr], Ret::F32),
    ("remquof", &[F32, F32, Ptr], Ret::F32), ("nan", &[Ptr], Ret::F64),
    ("sincos", &[F64, Ptr, Ptr], Ret::Void), ("sincosf", &[F32, Ptr, Ptr], Ret::Void),

    // Bytes and wide strings. wchar_t is 32 bits on both.
    ("memchr", &[Ptr, I32, U64], Ret::Int(Ptr)), ("memrchr", &[Ptr, I32, U64], Ret::Int(Ptr)),
    ("memcmp", &[Ptr, Ptr, U64], Ret::Int(I32)), ("memcpy", &[Ptr, Ptr, U64], Ret::Int(Ptr)),
    ("memmove", &[Ptr, Ptr, U64], Ret::Int(Ptr)), ("memset", &[Ptr, I32, U64], Ret::Int(Ptr)),
    ("strlen", &[Ptr], Ret::Int(U64)), ("strnlen", &[Ptr, U64], Ret::Int(U64)),
    ("strcmp", &[Ptr, Ptr], Ret::Int(I32)), ("strcasecmp", &[Ptr, Ptr], Ret::Int(I32)),
    ("strncmp", &[Ptr, Ptr, U64], Ret::Int(I32)), ("strncasecmp", &[Ptr, Ptr, U64], Ret::Int(I32)),
    ("strchr", &[Ptr, I32], Ret::Int(Ptr)), ("strrchr", &[Ptr, I32], Ret::Int(Ptr)),
    ("strstr", &[Ptr, Ptr], Ret::Int(Ptr)), ("strpbrk", &[Ptr, Ptr], Ret::Int(Ptr)),
    ("strspn", &[Ptr, Ptr], Ret::Int(U64)), ("strcspn", &[Ptr, Ptr], Ret::Int(U64)),
    ("strcpy", &[Ptr, Ptr], Ret::Int(Ptr)), ("strcat", &[Ptr, Ptr], Ret::Int(Ptr)),
    ("strncpy", &[Ptr, Ptr, U64], Ret::Int(Ptr)), ("strncat", &[Ptr, Ptr, U64], Ret::Int(Ptr)),
    ("strerror", &[I32], Ret::Int(Ptr)),
    ("wcslen", &[Ptr], Ret::Int(U64)), ("wmemchr", &[Ptr, U32, U64], Ret::Int(Ptr)),
    ("wmemcmp", &[Ptr, Ptr, U64], Ret::Int(I32)),
    ("atoi", &[Ptr], Ret::Int(I32)), ("atol", &[Ptr], Ret::Int(I64)), ("atoll", &[Ptr], Ret::Int(I64)),
    ("atof", &[Ptr], Ret::F64), ("strtod", &[Ptr, Ptr], Ret::F64), ("strtof", &[Ptr, Ptr], Ret::F32),
    ("strtol", &[Ptr, Ptr, I32], Ret::Int(I64)), ("strtoll", &[Ptr, Ptr, I32], Ret::Int(I64)),
    ("strtoul", &[Ptr, Ptr, I32], Ret::Int(U64)), ("strtoull", &[Ptr, Ptr, I32], Ret::Int(U64)),
    ("tolower", &[I32], Ret::Int(I32)), ("isspace", &[I32], Ret::Int(I32)),

    // FORTIFY. bionic's own (`__strlen_chk`, `__strchr_chk`, `__strncpy_chk2`,
    // `__write_chk`, `__readlink_chk`, `__fread_chk`) are Cordial's; the rest
    // glibc exports under the same names and contracts.
    ("__memcpy_chk", &[Ptr, Ptr, U64, U64], Ret::Int(Ptr)),
    ("__memmove_chk", &[Ptr, Ptr, U64, U64], Ret::Int(Ptr)),
    ("__memset_chk", &[Ptr, I32, U64, U64], Ret::Int(Ptr)),
    ("__strcpy_chk", &[Ptr, Ptr, U64], Ret::Int(Ptr)), ("__strcat_chk", &[Ptr, Ptr, U64], Ret::Int(Ptr)),
    ("__strncpy_chk", &[Ptr, Ptr, U64, U64], Ret::Int(Ptr)),
    ("__strncpy_chk2", &[Ptr, Ptr, U64, U64, U64], Ret::Int(Ptr)),
    ("__strlen_chk", &[Ptr, U64], Ret::Int(U64)), ("__strchr_chk", &[Ptr, I32, U64], Ret::Int(Ptr)),
    ("__read_chk", &[I32, Ptr, U64, U64], Ret::Int(I64)),
    ("__write_chk", &[I32, Ptr, U64, U64], Ret::Int(I64)),
    ("__readlink_chk", &[Ptr, Ptr, U64, U64], Ret::Int(I64)),
    ("__fread_chk", &[Ptr, U64, U64, Ptr, U64], Ret::Int(U64)),
    ("__FD_SET_chk", &[I32, Ptr, U64], Ret::Void), ("__FD_CLR_chk", &[I32, Ptr, U64], Ret::Void),
    ("__FD_ISSET_chk", &[I32, Ptr, U64], Ret::Int(I32)),
    ("__sendto_chk", &[I32, Ptr, U64, U64, I32, Ptr, U32], Ret::Int(I64)),
    ("__stack_chk_fail", &[], Ret::Void),

    // errno values are the same numbers on arm64 and x86-64 Linux.
    ("__errno", &[], Ret::Int(Ptr)),
    ("__assert", &[Ptr, I32, Ptr], Ret::Void), ("__assert2", &[Ptr, I32, Ptr, Ptr], Ret::Void),
    ("__gnu_strerror_r", &[I32, Ptr, U64], Ret::Int(Ptr)),
    ("abort", &[], Ret::Void),

    // Process, time, scheduling. clockid_t and the scheduling policies are
    // the generic numbers on both.
    ("getpid", &[], Ret::Int(I32)), ("getppid", &[], Ret::Int(I32)), ("gettid", &[], Ret::Int(I32)),
    ("getuid", &[], Ret::Int(U32)), ("geteuid", &[], Ret::Int(U32)),
    ("getpagesize", &[], Ret::Int(I32)), ("sched_yield", &[], Ret::Int(I32)),
    ("sched_getcpu", &[], Ret::Int(I32)),
    ("sched_get_priority_max", &[I32], Ret::Int(I32)), ("sched_get_priority_min", &[I32], Ret::Int(I32)),
    ("sched_getscheduler", &[I32], Ret::Int(I32)), ("sched_getparam", &[I32, Ptr], Ret::Int(I32)),
    ("sched_setscheduler", &[I32, I32, Ptr], Ret::Int(I32)),
    ("getpriority", &[I32, U32], Ret::Int(I32)), ("setpriority", &[I32, U32, I32], Ret::Int(I32)),
    ("clock", &[], Ret::Int(I64)), ("time", &[Ptr], Ret::Int(I64)),
    ("difftime", &[I64, I64], Ret::F64),
    ("clock_gettime", &[I32, Ptr], Ret::Int(I32)), ("gettimeofday", &[Ptr, Ptr], Ret::Int(I32)),
    ("nanosleep", &[Ptr, Ptr], Ret::Int(I32)), ("usleep", &[U32], Ret::Int(I32)),
    ("localtime_r", &[Ptr, Ptr], Ret::Int(Ptr)), ("gmtime_r", &[Ptr, Ptr], Ret::Int(Ptr)),
    ("localtime", &[Ptr], Ret::Int(Ptr)), ("gmtime", &[Ptr], Ret::Int(Ptr)),
    ("mktime", &[Ptr], Ret::Int(I64)), ("tzset", &[], Ret::Void),
    ("strftime", &[Ptr, U64, Ptr, Ptr], Ret::Int(U64)),
    ("getenv", &[Ptr], Ret::Int(Ptr)), ("gethostname", &[Ptr, U64], Ret::Int(I32)),
    // Cordial's property table names no instruction set, so the phone
    // build's answers are the guest's too; an absent key is "", as unset.
    ("__system_property_get", &[Ptr, Ptr], Ret::Int(I32)),
    ("rand", &[], Ret::Int(I32)), ("srand", &[U32], Ret::Void),
    ("arc4random_buf", &[Ptr, U64], Ret::Void), ("getentropy", &[Ptr, U64], Ret::Int(I32)),
    ("getopt_long", &[I32, Ptr, Ptr, Ptr, Ptr], Ret::Int(I32)),
    ("openlog", &[Ptr, I32, I32], Ret::Void), ("closelog", &[], Ret::Void),

    // Descriptors and paths. `open` is not here: its O_* flags differ.
    ("close", &[I32], Ret::Int(I32)), ("fsync", &[I32], Ret::Int(I32)),
    ("read", &[I32, Ptr, U64], Ret::Int(I64)), ("write", &[I32, Ptr, U64], Ret::Int(I64)),
    ("pread", &[I32, Ptr, U64, I64], Ret::Int(I64)), ("pread64", &[I32, Ptr, U64, I64], Ret::Int(I64)),
    ("pwrite", &[I32, Ptr, U64, I64], Ret::Int(I64)), ("lseek", &[I32, I64, I32], Ret::Int(I64)),
    ("ftruncate", &[I32, I64], Ret::Int(I32)), ("fchmod", &[I32, U32], Ret::Int(I32)),
    ("fchown", &[I32, U32, U32], Ret::Int(I32)), ("posix_fallocate", &[I32, I64, I64], Ret::Int(I32)),
    ("access", &[Ptr, I32], Ret::Int(I32)), ("mkdir", &[Ptr, U32], Ret::Int(I32)),
    ("rmdir", &[Ptr], Ret::Int(I32)), ("unlink", &[Ptr], Ret::Int(I32)), ("remove", &[Ptr], Ret::Int(I32)),
    ("rename", &[Ptr, Ptr], Ret::Int(I32)), ("readlink", &[Ptr, Ptr, U64], Ret::Int(I64)),
    ("realpath", &[Ptr, Ptr], Ret::Int(Ptr)), ("getcwd", &[Ptr, U64], Ret::Int(Ptr)),
    ("utime", &[Ptr, Ptr], Ret::Int(I32)), ("utimes", &[Ptr, Ptr], Ret::Int(I32)),
    ("opendir", &[Ptr], Ret::Int(Ptr)), ("readdir", &[Ptr], Ret::Int(Ptr)), ("closedir", &[Ptr], Ret::Int(I32)),
    // Cordial's statvfs fills bionic's LP64 struct statvfs, which arm64
    // shares with x86-64 (system_paths.cpp).
    ("statvfs", &[Ptr, Ptr], Ret::Int(I32)),
    ("pipe", &[Ptr], Ret::Int(I32)), ("eventfd", &[U32, I32], Ret::Int(I32)),
    ("timerfd_create", &[I32, I32], Ret::Int(I32)),
    ("timerfd_settime", &[I32, I32, Ptr, Ptr], Ret::Int(I32)),
    ("poll", &[Ptr, U64, I32], Ret::Int(I32)),
    ("mlock", &[Ptr, U64], Ret::Int(I32)),
    // The kernel's own LP64 structures, which glibc and arm64 bionic both
    // pass through unchanged: struct sysinfo (112 bytes), struct iovec,
    // fd_set with struct timeval, struct msghdr/mmsghdr/cmsghdr.
    ("sysinfo", &[Ptr], Ret::Int(I32)), ("writev", &[I32, Ptr, I32], Ret::Int(I64)),
    ("select", &[I32, Ptr, Ptr, Ptr, Ptr], Ret::Int(I32)),
    ("msync", &[Ptr, U64, I32], Ret::Int(I32)),

    // Sockets. sockaddr and the SOCK_/SOL_/SO_ numbers are the generic ones.
    ("socket", &[I32, I32, I32], Ret::Int(I32)), ("socketpair", &[I32, I32, I32, Ptr], Ret::Int(I32)),
    ("connect", &[I32, Ptr, U32], Ret::Int(I32)), ("bind", &[I32, Ptr, U32], Ret::Int(I32)),
    ("listen", &[I32, I32], Ret::Int(I32)), ("accept", &[I32, Ptr, Ptr], Ret::Int(I32)),
    ("accept4", &[I32, Ptr, Ptr, I32], Ret::Int(I32)), ("shutdown", &[I32, I32], Ret::Int(I32)),
    ("sendto", &[I32, Ptr, U64, I32, Ptr, U32], Ret::Int(I64)),
    ("recvfrom", &[I32, Ptr, U64, I32, Ptr, Ptr], Ret::Int(I64)),
    ("getsockopt", &[I32, I32, I32, Ptr, Ptr], Ret::Int(I32)),
    ("setsockopt", &[I32, I32, I32, Ptr, U32], Ret::Int(I32)),
    ("getsockname", &[I32, Ptr, Ptr], Ret::Int(I32)), ("getpeername", &[I32, Ptr, Ptr], Ret::Int(I32)),
    ("sendmsg", &[I32, Ptr, I32], Ret::Int(I64)), ("recvmsg", &[I32, Ptr, I32], Ret::Int(I64)),
    ("sendmmsg", &[I32, Ptr, U32, I32], Ret::Int(I32)), ("recvmmsg", &[I32, Ptr, U32, I32, Ptr], Ret::Int(I32)),
    ("inet_pton", &[I32, Ptr, Ptr], Ret::Int(I32)), ("inet_ntop", &[I32, Ptr, Ptr, U32], Ret::Int(Ptr)),
    ("if_nametoindex", &[Ptr], Ret::Int(U32)), ("if_indextoname", &[U32, Ptr], Ret::Int(Ptr)),
    ("__cmsg_nxthdr", &[Ptr, Ptr], Ret::Int(Ptr)),
    ("getaddrinfo", &[Ptr, Ptr, Ptr, Ptr], Ret::Int(I32)), ("freeaddrinfo", &[Ptr], Ret::Void),
    ("gai_strerror", &[I32], Ret::Int(Ptr)), ("gethostbyname", &[Ptr], Ret::Int(Ptr)),
    ("sysconf", &[I32], Ret::Int(I64)),

    // stdio on FILE*. The guest only passes these pointers back.
    ("fopen", &[Ptr, Ptr], Ret::Int(Ptr)), ("fdopen", &[I32, Ptr], Ret::Int(Ptr)),
    ("fclose", &[Ptr], Ret::Int(I32)), ("fflush", &[Ptr], Ret::Int(I32)),
    ("fread", &[Ptr, U64, U64, Ptr], Ret::Int(U64)), ("fwrite", &[Ptr, U64, U64, Ptr], Ret::Int(U64)),
    ("fseek", &[Ptr, I64, I32], Ret::Int(I32)), ("fseeko", &[Ptr, I64, I32], Ret::Int(I32)),
    ("ftell", &[Ptr], Ret::Int(I64)), ("ftello", &[Ptr], Ret::Int(I64)),
    ("fputs", &[Ptr, Ptr], Ret::Int(I32)), ("fputc", &[I32, Ptr], Ret::Int(I32)),
    ("puts", &[Ptr], Ret::Int(I32)), ("fgets", &[Ptr, I32, Ptr], Ret::Int(Ptr)),
    ("getc", &[Ptr], Ret::Int(I32)), ("ungetc", &[I32, Ptr], Ret::Int(I32)),
    ("fileno", &[Ptr], Ret::Int(I32)), ("feof", &[Ptr], Ret::Int(I32)), ("ferror", &[Ptr], Ret::Int(I32)),
    ("clearerr", &[Ptr], Ret::Void), ("setvbuf", &[Ptr, Ptr, I32, U64], Ret::Int(I32)),

    // pthreads, excluding every entry point that takes a guest callback or
    // describes the calling thread's stack. bionic's LP64 pthread types are
    // the same bytes on arm64 and x86-64.
    ("pthread_self", &[], Ret::Int(U64)), ("pthread_equal", &[U64, U64], Ret::Int(I32)),
    ("pthread_detach", &[U64], Ret::Int(I32)), ("pthread_join", &[U64, Ptr], Ret::Int(I32)),
    ("pthread_setname_np", &[U64, Ptr], Ret::Int(I32)),
    ("pthread_getschedparam", &[U64, Ptr, Ptr], Ret::Int(I32)),
    ("pthread_setschedparam", &[U64, I32, Ptr], Ret::Int(I32)),
    ("pthread_getspecific", &[U32], Ret::Int(Ptr)), ("pthread_setspecific", &[U32, Ptr], Ret::Int(I32)),
    ("pthread_key_delete", &[U32], Ret::Int(I32)),
    ("pthread_attr_init", &[Ptr], Ret::Int(I32)), ("pthread_attr_destroy", &[Ptr], Ret::Int(I32)),
    ("pthread_attr_setdetachstate", &[Ptr, I32], Ret::Int(I32)),
    ("pthread_attr_setstacksize", &[Ptr, U64], Ret::Int(I32)),
    ("pthread_attr_setschedparam", &[Ptr, Ptr], Ret::Int(I32)),
    ("pthread_mutex_init", &[Ptr, Ptr], Ret::Int(I32)), ("pthread_mutex_destroy", &[Ptr], Ret::Int(I32)),
    ("pthread_mutex_lock", &[Ptr], Ret::Int(I32)), ("pthread_mutex_trylock", &[Ptr], Ret::Int(I32)),
    ("pthread_mutex_unlock", &[Ptr], Ret::Int(I32)),
    ("pthread_mutexattr_init", &[Ptr], Ret::Int(I32)), ("pthread_mutexattr_destroy", &[Ptr], Ret::Int(I32)),
    ("pthread_mutexattr_settype", &[Ptr, I32], Ret::Int(I32)),
    ("pthread_cond_init", &[Ptr, Ptr], Ret::Int(I32)), ("pthread_cond_destroy", &[Ptr], Ret::Int(I32)),
    ("pthread_cond_signal", &[Ptr], Ret::Int(I32)), ("pthread_cond_broadcast", &[Ptr], Ret::Int(I32)),
    ("pthread_cond_wait", &[Ptr, Ptr], Ret::Int(I32)),
    ("pthread_cond_timedwait", &[Ptr, Ptr, Ptr], Ret::Int(I32)),
    ("pthread_condattr_init", &[Ptr], Ret::Int(I32)), ("pthread_condattr_destroy", &[Ptr], Ret::Int(I32)),
    ("pthread_condattr_setclock", &[Ptr, I32], Ret::Int(I32)),
    ("pthread_rwlock_init", &[Ptr, Ptr], Ret::Int(I32)), ("pthread_rwlock_destroy", &[Ptr], Ret::Int(I32)),
    ("pthread_rwlock_rdlock", &[Ptr], Ret::Int(I32)), ("pthread_rwlock_wrlock", &[Ptr], Ret::Int(I32)),
    ("pthread_rwlock_unlock", &[Ptr], Ret::Int(I32)),

    // liblog and libandroid, as Cordial implements them. Opaque handles, and
    // strings.
    ("__android_log_write", &[I32, Ptr, Ptr], Ret::Int(I32)),
    ("__android_log_buf_write", &[I32, I32, Ptr, Ptr], Ret::Int(I32)),
    ("android_set_abort_message", &[Ptr], Ret::Void),
    ("AAsset_close", &[Ptr], Ret::Void), ("AAsset_getBuffer", &[Ptr], Ret::Int(Ptr)),
    ("AAsset_getLength", &[Ptr], Ret::Int(I64)), ("AAssetManager_open", &[Ptr, Ptr, I32], Ret::Int(Ptr)),
    // off_t is 64-bit on LP64 bionic, and the descriptor is a host memfd the
    // guest's read/lseek/mmap reach unchanged.
    ("AAsset_openFileDescriptor", &[Ptr, Ptr, Ptr], Ret::Int(I32)),
    ("AConfiguration_new", &[], Ret::Int(Ptr)), ("AConfiguration_delete", &[Ptr], Ret::Void),
    ("AConfiguration_fromAssetManager", &[Ptr, Ptr], Ret::Void),
    ("AConfiguration_getCountry", &[Ptr, Ptr], Ret::Void),
    ("AConfiguration_getLanguage", &[Ptr, Ptr], Ret::Void),
    ("AConfiguration_getNavHidden", &[Ptr], Ret::Int(I32)),
    ("AConfiguration_getScreenHeightDp", &[Ptr], Ret::Int(I32)),
    ("AConfiguration_getScreenWidthDp", &[Ptr], Ret::Int(I32)),
    ("AConfiguration_getScreenSize", &[Ptr], Ret::Int(I32)),
    ("ANativeWindow_acquire", &[Ptr], Ret::Void), ("ANativeWindow_release", &[Ptr], Ret::Void),
    ("ANativeWindow_getWidth", &[Ptr], Ret::Int(I32)), ("ANativeWindow_getHeight", &[Ptr], Ret::Int(I32)),
    ("ALooper_forThread", &[], Ret::Int(Ptr)), ("ALooper_prepare", &[I32], Ret::Int(Ptr)),
    ("ALooper_acquire", &[Ptr], Ret::Void), ("ALooper_release", &[Ptr], Ret::Void),
    ("ALooper_removeFd", &[Ptr, I32], Ret::Int(I32)),
    // Cordial's looper calls back only through host entries: the guest's
    // callbacks are wrapped when registered (`guest_sys`, `ALooper_addFd`).
    ("ALooper_pollOnce", &[I32, Ptr, Ptr, Ptr], Ret::Int(I32)),

    // M4. Cordial's semaphores wrap bionic's 16-byte sem_t, the same size on
    // arm64 (`bionic/pthread.rs`); epoll's descriptors take flags whose
    // numbers are the same (EPOLL_CLOEXEC is O_CLOEXEC on both); the
    // attr getters read an attr the host filled (`guest_sys`).
    ("sem_init", &[Ptr, I32, U32], Ret::Int(I32)), ("sem_destroy", &[Ptr], Ret::Int(I32)),
    ("sem_post", &[Ptr], Ret::Int(I32)), ("sem_wait", &[Ptr], Ret::Int(I32)),
    ("sem_trywait", &[Ptr], Ret::Int(I32)),
    ("epoll_create", &[I32], Ret::Int(I32)), ("epoll_create1", &[I32], Ret::Int(I32)),
    ("pthread_attr_getstack", &[Ptr, Ptr, Ptr], Ret::Int(I32)),
    ("pthread_attr_getstacksize", &[Ptr, Ptr], Ret::Int(I32)),
    ("pthread_attr_getguardsize", &[Ptr, Ptr], Ret::Int(I32)),
];

/// Where only Cordial's translation is right, because the host's own
/// constants differ from bionic's: `AI_*`/`EAI_*` for the resolver, the
/// `_SC_*` selectors for `sysconf`. Answered by the host, these would be a
/// wrong answer rather than a missing one, so they stop instead.
const CORDIAL_ONLY: &[&str] = &["getaddrinfo", "freeaddrinfo", "gai_strerror", "sysconf"];

/// Weak imports that bionic does not define either, so on the Quest they are
/// null and the engine checks for that. A stub would turn "not present" into
/// a call that stops.
const WEAK_ABSENT_ON_ANDROID: &[&str] = &["__gcov_dump", "__gcov_flush"];

/// Why a function has no dispatch yet, by what is missing. The first match
/// wins. This is the M3 backlog in the order the design names it.
fn stop_reason(name: &str) -> String {
    if let Some(why) = named_gap(name) {
        return why.to_string();
    }
    if name.ends_with("_l") || name.starts_with("isw") || name.starts_with("mb") || name.starts_with("wc") {
        return "locale or multibyte state, which differs between bionic and glibc".into();
    }
    if name.starts_with("ovr_") {
        return "Meta platform: not one of the names guest_ovr answers".into();
    }
    if let Some((_, why)) = gl_table::REFUSED.iter().find(|(n, _)| *n == name) {
        return format!("GLES/EGL: {why}, which the call builder cannot move");
    }
    if name.starts_with("gl") || name.starts_with("egl") {
        return "GLES/EGL: not in the generated table (tools/vr/gen-guest-gl.py)".into();
    }
    if name.starts_with("AMedia") {
        return "NDK media: Cordial's AMedia answers are stubs on the native path too".into();
    }
    "no signature written for the call builder yet".into()
}

/// The gaps named one function at a time.
fn named_gap(name: &str) -> Option<&'static str> {
    let groups: &[(&[&str], &str)] = &[
        (&["fmal", "powl"],
         "long double is a 128-bit quad on arm64 and 80-bit x87 on x86-64 (design §3.1)"),
        (&["__register_atfork"],
         "takes or runs a guest callback, which needs a host-to-guest trampoline (design §3.1)"),
        (&["raise"],
         "signals are held virtually for the guest, and delivering one to a guest handler is not built (design §4)"),
        (&["ptrace", "mremap"],
         "variadic, and the numbers or requests are arm64's (design §3.1)"),
        (&["fork", "execv", "execve", "waitpid", "exit", "_exit", "_Exit"],
         "process lifecycle: guest atexit handlers, and an arm64 execve must fail with ENOEXEC (design §4)"),
        (&["tcgetattr", "tcsetattr"], "struct termios differs between bionic and glibc"),
    ];
    groups.iter().find(|(names, _)| names.contains(&name)).map(|(_, why)| *why)
}

/// A stub that stops the guest, naming itself and the gap.
fn stop(name: &str, why: String) -> Handler {
    let name = name.to_owned();
    Box::new(move |_| Err(Fault::Unsupported { thunk: name.clone(), why: why.clone() }))
}

/// `eglGetProcAddress(name)`: the native answer, which is a host function,
/// handed to the guest as a stub whose signature comes from the generated
/// table -- made once per name. A name the table does not cover gets null,
/// which is what EGL returns for a function it does not have, and is said
/// once on stderr: the guest would otherwise jump to x86 code.
fn egl_get_proc_address(native: usize) -> Handler {
    use std::sync::Mutex;
    static MADE: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());
    Box::new(move |c| {
        // SAFETY: the guest's C string; identity mapping.
        let cname = unsafe { std::ffi::CStr::from_ptr(c.x(0) as *const c_char) };
        let name = cname.to_string_lossy().into_owned();
        if let Some(&stub) = MADE.lock().unwrap().get(&name) {
            c.set_x(0, stub);
            return Ok(());
        }
        // SAFETY: the native eglGetProcAddress with the guest's C string.
        let out = unsafe { cordial_guest::invoke("eglGetProcAddress", native as *const c_void, &[Ptr], &[c.x(0)]) }?;
        let host = out.rax;
        let sig = gl_table::GL.iter().find(|(n, _, _)| *n == name);
        let stub = match (host, sig) {
            (0, _) => 0,
            (h, Some(&(_, args, ret))) => {
                let h = h as usize;
                let rt = c.runtime();
                let st = rt.register(&name, Box::new(move |c| c.host(h as *const c_void, args, ret)));
                MADE.lock().unwrap().insert(name, st);
                st
            }
            (_, None) => {
                eprintln!("[guest] eglGetProcAddress({name}): the host has it but its signature is not in the \
                           generated table; answered null");
                0
            }
        };
        c.set_x(0, stub);
        Ok(())
    })
}

/// `const char*` storage with a C string behind it.
#[repr(transparent)]
struct CStrPtr(*const c_char);
// SAFETY: points at a 'static string literal and is never written.
unsafe impl Sync for CStrPtr {}

/// libmediandk's `AMEDIAFORMAT_KEY_*` are `const char*` variables. Their
/// values are the keys `android.media.MediaFormat` documents, which is what
/// the Quest's libmediandk holds. The native path has no data for these at
/// all; it is not a precedent.
static AMEDIAFORMAT_KEYS: [(&str, CStrPtr); 10] = [
    ("AMEDIAFORMAT_KEY_MIME", CStrPtr(c"mime".as_ptr())),
    ("AMEDIAFORMAT_KEY_WIDTH", CStrPtr(c"width".as_ptr())),
    ("AMEDIAFORMAT_KEY_HEIGHT", CStrPtr(c"height".as_ptr())),
    ("AMEDIAFORMAT_KEY_COLOR_FORMAT", CStrPtr(c"color-format".as_ptr())),
    ("AMEDIAFORMAT_KEY_STRIDE", CStrPtr(c"stride".as_ptr())),
    ("AMEDIAFORMAT_KEY_BIT_RATE", CStrPtr(c"bitrate".as_ptr())),
    ("AMEDIAFORMAT_KEY_FRAME_RATE", CStrPtr(c"frame-rate".as_ptr())),
    ("AMEDIAFORMAT_KEY_I_FRAME_INTERVAL", CStrPtr(c"i-frame-interval".as_ptr())),
    ("AMEDIAFORMAT_KEY_CHANNEL_COUNT", CStrPtr(c"channel-count".as_ptr())),
    ("AMEDIAFORMAT_KEY_SAMPLE_RATE", CStrPtr(c"sample-rate".as_ptr())),
];

/// Host storage for a data import, and what it is.
///
/// Only names whose layout is known to match bionic's on arm64 are answered;
/// anything else is `None`, and the load fails naming it.
fn data_answer(name: &str, native: Option<&(String, *mut c_void, Source)>) -> Option<(*mut c_void, String)> {
    if let Some((_, v)) = AMEDIAFORMAT_KEYS.iter().find(|(n, _)| *n == name) {
        return Some((v as *const CStrPtr as *mut c_void, "const char* to MediaFormat's key string".into()));
    }
    let what = match name {
        // Cordial's own. `__sF` is three 152-byte legacy FILEs, which is
        // sizeof(struct __sFILE) on every LP64 bionic; Cordial's FILE
        // functions map them to the host's streams. The guard is one word;
        // M3 must put the same value in each guest thread's TLS slot 5.
        "__sF" => "Cordial's legacy __sF[3], LP64 layout",
        "__stack_chk_guard" => "Cordial's canary word",
        // The host's own variables, each a pointer, an int, a long or a
        // 16-byte in6_addr: the same on both sides. The functions that read
        // or write them (getopt_long, tzset) run on the host too, so each
        // pair stays consistent.
        "stdin" | "stdout" | "stderr" => "host FILE* variable",
        "environ" => "host char** variable",
        "optarg" | "optind" => "host getopt state",
        "tzname" | "daylight" | "timezone" => "host tzset state",
        "in6addr_any" | "in6addr_loopback" => "host struct in6_addr",
        _ => return None,
    };
    match native {
        Some((_, addr, Source::Cordial | Source::Host)) => Some((*addr, what.into())),
        _ => None,
    }
}

/// Builds the guest's table. `native` is the table the x86-64 path would
/// use, and decides which implementation a dispatching stub calls;
/// `imports` and `data` are the engine's own (`elf::undefined_symbols`,
/// `elf::undefined_data_symbols`); `needed` its `DT_NEEDED` list, each of
/// which is registered even when empty so the linker never goes looking for
/// the real file; `omit` is left out entirely, which is the control that a
/// missing stub fails the load by name.
pub fn build(
    rt: &Arc<Runtime>,
    native: &SymbolTable,
    imports: &Imports,
    data: &BTreeSet<String>,
    needed: &[String],
    omit: &BTreeSet<String>,
) -> GuestTable {
    let mut answers: BTreeMap<&str, (String, *mut c_void, Source)> = BTreeMap::new();
    for (lib, entries) in &native.libraries {
        for e in entries {
            answers.entry(e.symbol).or_insert(((*lib).to_string(), e.address, e.source));
        }
    }
    // The generated EGL/GLES signatures (§9.3) dispatch exactly like the
    // hand-written ones: to whatever the native table resolves, which for
    // these is the host's libEGL/libGLESv2 or Cordial's EGL overrides.
    let funcs: BTreeMap<&str, (&'static [Ty], Ret)> = FUNCS.iter().map(|(n, a, r)| (*n, (*a, *r)))
        .chain(gl_table::GL.iter().map(|(n, a, r)| (*n, (*a, *r))))
        .collect();

    let mut table = GuestTable { libraries: BTreeMap::new(), rows: Vec::new() };
    for lib in needed {
        // libdl.so is the linker's own, already loaded, and host: its names
        // go in the guest libc.so below instead, where a guest can bind them.
        if lib != "libdl.so" {
            table.libraries.entry(lib.clone()).or_default();
        }
    }

    let mut key_fns: Option<(u64, u64)> = None;
    for (name, binding) in imports {
        let native_answer = answers.get(name.as_str());
        let library = if name.starts_with("xr") {
            OPENXR_LOADER.to_string()
        } else if name.starts_with("ovr_") {
            OVR_PLATFORM_LOADER.to_string()
        } else if let Some((lib, _, _)) = native_answer {
            lib.clone()
        } else {
            symtab::library_for(name).to_string()
        };
        let mut row = Row { symbol: name.clone(), library: library.clone(), answer: Answer::Stop, note: String::new() };

        if omit.contains(name) {
            row.answer = Answer::Unanswered;
            row.note = "left out by CORDIAL_GUEST_OMIT".into();
            table.rows.push(row);
            continue;
        }
        if *binding == Binding::Weak && WEAK_ABSENT_ON_ANDROID.contains(&name.as_str()) {
            row.answer = Answer::WeakNull;
            row.note = "weak, and bionic does not define it either".into();
            table.rows.push(row);
            continue;
        }

        let address = if data.contains(name) {
            match data_answer(name, native_answer) {
                Some((addr, what)) => {
                    row.answer = Answer::Data;
                    row.note = what;
                    Some(addr)
                }
                None => {
                    row.answer = Answer::Unanswered;
                    row.note = "a data import with no storage laid out for arm64 bionic".into();
                    None
                }
            }
        } else if crate::guest_libc::guest_keys()
            && matches!(name.as_str(), "pthread_getspecific" | "pthread_setspecific")
        {
            // Guest code, not a stub: bionic's table lookup through
            // TPIDR_EL0, with no SVC (cordial-guest keys.rs, M7).
            let (get, set) = *key_fns.get_or_insert_with(|| rt.key_functions());
            row.answer = Answer::Thunk;
            row.note = "bionic's key lookup as guest code in the stub page (cordial-guest keys.rs)".into();
            Some((if name == "pthread_getspecific" { get } else { set }) as *mut c_void)
        } else {
            let (handler, answer, note) = function_handler(rt, &answers, name, native_answer, &funcs);
            row.answer = answer;
            row.note = note;
            let stub = rt.register(name, handler);
            // Short copies, fills and compares, and CLOCK_MONOTONIC from the
            // counter, as guest code that falls back to the stub just
            // registered for everything else (cordial-guest string.rs and
            // clock.rs, M7).
            if crate::guest_libc::guest_string() && cordial_guest::STRING_FUNCTIONS.contains(&name.as_str()) {
                row.note = format!("guest code up to 128 bytes, then {}", row.note);
                rt.string_function(name, stub).map(|a| a as *mut c_void)
            } else if crate::guest_libc::guest_clock() && name == "clock_gettime" {
                row.note = format!("guest code for CLOCK_MONOTONIC, then {}", row.note);
                Some(rt.clock_function(stub) as *mut c_void)
            } else {
                Some(stub as *mut c_void)
            }
        };

        if let Some(addr) = address {
            table.libraries.entry(library).or_default().push((name.clone(), addr));
        }
        table.rows.push(row);
    }
    // Vulkan is not imported; the engine `dlopen`s it (§3.2, M5). The guest
    // library exports what the native virtual one does, `vkGetInstanceProcAddr`
    // alone, over the same Cordial implementation, so `VK_KHR_android_surface`,
    // the swapchain's present mode and the capture keep working behind it.
    let native_gipa = native.libraries.iter()
        .filter(|(lib, _)| crate::android::vulkan::LIBRARY_NAMES.contains(lib))
        .flat_map(|(_, entries)| entries.iter())
        .find(|e| e.symbol == "vkGetInstanceProcAddr" && e.source == Source::Cordial)
        .map(|e| e.address as usize);
    if let Some(gipa) = native_gipa {
        let stub = rt.register("vkGetInstanceProcAddr", crate::guest_vk::get_instance_proc_addr(gipa));
        for lib in crate::android::vulkan::LIBRARY_NAMES {
            table.libraries.entry(lib.to_string()).or_default().push(("vkGetInstanceProcAddr".into(), stub as *mut c_void));
        }
    }
    // AAudio is not imported either; FMOD `dlopen`s it once
    // `supportsAAudio()` has said yes, and finding nothing there it fails
    // `System::init` with no fallback (guest_audio).
    if let Some(syms) = crate::guest_audio::library(rt, native) {
        table.libraries.insert(symtab::AAUDIO_LIBRARY_NAME.to_string(), syms);
    }
    let data_addrs = table.rows.iter().filter(|r| r.answer == Answer::Data)
        .filter_map(|r| table.libraries.get(&r.library)?.iter().find(|(n, _)| n == &r.symbol))
        .map(|(_, a)| *a as u64)
        .collect();
    let _ = crate::guest_libc::GUEST_DATA.set(data_addrs);
    table
}

/// The stub's handler for a function import, how it is answered, and a note.
fn function_handler(
    rt: &Arc<Runtime>,
    answers: &BTreeMap<&str, (String, *mut c_void, Source)>,
    name: &str,
    native: Option<&(String, *mut c_void, Source)>,
    funcs: &BTreeMap<&str, (&'static [Ty], Ret)>,
) -> (Handler, Answer, String) {
    if name.starts_with("xr") {
        return (crate::guest_xr::import(name), Answer::OpenXr,
                "bridged to the host's OpenXR loader (guest_xr)".into());
    }
    if name.starts_with("ovr_") {
        if let Some(h) = crate::guest_ovr::handler(name) {
            return (h, Answer::Thunk, "fails honestly: no Meta platform services on this host (guest_ovr)".into());
        }
    }
    let native_of = |n: &str| match answers.get(n) {
        Some((_, addr, Source::Cordial | Source::Host)) => Some(*addr as usize),
        _ => None,
    };
    if let Some((h, note)) = crate::guest_libc::handler(name, rt, &native_of) {
        return (h, Answer::Thunk, note.into());
    }
    // What the native path would call, if it is a real implementation rather
    // than one of Cordial's generated x86-64 stubs.
    let implemented = match native {
        Some((_, addr, src @ (Source::Cordial | Source::Host))) => Some((*addr, *src)),
        _ => None,
    };
    let answer_for = |src: Source| if src == Source::Cordial { Answer::Cordial } else { Answer::Host };

    let printf: Option<(&'static [Ty], usize)> = match name {
        "printf" => Some((&[Ptr], 0)),
        "snprintf" => Some((&[Ptr, U64, Ptr], 2)),
        "fprintf" => Some((&[Ptr, Ptr], 1)),
        "__android_log_print" => Some((&[I32, Ptr, Ptr], 2)),
        "__android_log_assert" => Some((&[Ptr, Ptr, Ptr], 2)),
        "syslog" => Some((&[I32, Ptr], 1)),
        _ => None,
    };
    if let Some((named, fmt)) = printf {
        return match implemented {
            Some((addr, src)) => {
                let label: &'static str = Box::leak(name.to_owned().into_boxed_str());
                (thunks::printf_like(label, addr, named, fmt), Answer::Thunk,
                 format!("printf-family thunk over the {} implementation", answer_for(src).label()))
            }
            None => (stop(name, "no implementation on the native path either".into()), Answer::Stop,
                     "no implementation on the native path either".into()),
        };
    }
    if name == "eglGetProcAddress" {
        return match native_of("eglGetProcAddress") {
            Some(f) => (egl_get_proc_address(f), Answer::Thunk,
                        "the native answer, each function behind a stub made from its generated signature".into()),
            None => (stop(name, "no native eglGetProcAddress".into()), Answer::Stop,
                     "no native eglGetProcAddress".into()),
        };
    }
    if name == "qsort" {
        return (thunks::qsort(), Answer::Thunk, "host qsort_r calling the guest comparator".into());
    }

    if let Some(&(args, ret)) = funcs.get(name) {
        return match implemented {
            Some((_, Source::Host)) if CORDIAL_ONLY.contains(&name) => {
                let why = "the host's constants differ from bionic's and Cordial's translation is not \
                           what the native table picked"
                    .to_string();
                (stop(name, why.clone()), Answer::Stop, why)
            }
            Some((addr, src)) => {
                let f = addr as usize;
                let h: Handler = Box::new(move |c| c.host(f as *const c_void, args, ret));
                (h, answer_for(src), String::new())
            }
            None => {
                let why = "the native path has no implementation either (a stub, or --host-libc is off)".to_string();
                (stop(name, why.clone()), Answer::Stop, why)
            }
        };
    }
    let why = stop_reason(name);
    (stop(name, why.clone()), Answer::Stop, why)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_described_function_is_named_once() {
        let mut seen = BTreeSet::new();
        for (n, _, _) in FUNCS {
            assert!(seen.insert(*n), "{n} is described twice");
        }
    }

    /// The stop groups and the dispatch table must not overlap: a name in
    /// both would dispatch while its stop reason claimed it could not.
    fn table_for(names: &[(&str, Binding)], data: &[&str], omit: &[&str]) -> (Arc<Runtime>, GuestTable) {
        let imports: Imports = names.iter().map(|(n, b)| (n.to_string(), *b)).collect();
        let data: BTreeSet<String> = data.iter().map(|s| s.to_string()).collect();
        let omit: BTreeSet<String> = omit.iter().map(|s| s.to_string()).collect();
        let native = symtab::build(true, &imports);
        let rt = Runtime::new(cordial_guest::Options::default());
        let t = build(&rt, &native, &imports, &data, &["libc.so".into(), "libdl.so".into()], &omit);
        (rt, t)
    }

    fn address(t: &GuestTable, name: &str) -> Option<u64> {
        t.libraries.values().flatten().find(|(n, _)| n == name).map(|(_, a)| *a as u64)
    }

    /// The three kinds of function stub, each entered the way the guest
    /// enters it: through the translator, at the stub's address.
    #[test]
    fn stubs_dispatch_fail_honestly_or_stop_by_name() {
        let (rt, t) = table_for(
            &[("strlen", Binding::Strong), ("xrCreateInstance", Binding::Strong),
              ("fork", Binding::Strong), ("ovr_PopMessage", Binding::Strong)],
            &[], &[]);
        let (page, end) = rt.stub_page();
        for name in ["strlen", "xrCreateInstance", "fork", "ovr_PopMessage"] {
            let a = address(&t, name).unwrap_or_else(|| panic!("{name} not registered"));
            assert!((page..end).contains(&a), "{name} resolved outside the stub page");
        }
        assert_eq!(t.libraries[OPENXR_LOADER].len(), 1);
        assert_eq!(t.libraries[OVR_PLATFORM_LOADER].len(), 1);
        assert!(!t.libraries.contains_key("libdl.so"), "libdl.so is the linker's own, and host");

        let s = c"twelve bytes";
        let r = cordial_guest::guest_call(&rt, address(&t, "strlen").unwrap(), &[s.as_ptr() as u64], &[]).unwrap();
        assert_eq!(r.x0, 12);

        // A null create info: the bridge refuses it itself, and so does the
        // no-loader answer, so the result is a failure either way.
        let r = cordial_guest::guest_call(&rt, address(&t, "xrCreateInstance").unwrap(), &[0, 0], &[]).unwrap();
        assert!([-1, -51].contains(&(r.x0 as i32)), "xrCreateInstance(NULL) -> {}", r.x0 as i32);

        // The Meta platform loader answers, with nothing queued.
        let _turn = crate::guest_ovr::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let r = cordial_guest::guest_call(&rt, address(&t, "ovr_PopMessage").unwrap(), &[], &[]).unwrap();
        assert_eq!(r.x0, 0, "ovr_PopMessage on an empty queue is null");

        for name in ["fork"] {
            match cordial_guest::guest_call(&rt, address(&t, name).unwrap(), &[], &[]) {
                Err(Fault::Unsupported { thunk, .. }) => assert_eq!(thunk, name),
                other => panic!("{name} did not stop by name: {other:?}"),
            }
        }
    }

    /// ADR-053's first condition, from the table's side: every address a
    /// function import resolves to is one of the runtime's registered stubs,
    /// so the guest can only ever reach a handler through Cordial's own stub
    /// page. Run over every import the x86-64 engine has
    /// (`docs/analysis/undefined-symbols.tsv`), every described signature,
    /// and the OpenXR and Meta loaders' names, so each kind of answer is in it.
    #[test]
    fn every_function_import_resolves_to_a_registered_stub() {
        let tsv = include_str!("../../../docs/analysis/undefined-symbols.tsv");
        let mut names: BTreeSet<&str> = tsv.lines().filter_map(|l| l.split('\t').nth(1)).collect();
        names.extend(FUNCS.iter().map(|(n, _, _)| *n));
        names.extend(gl_table::GL.iter().map(|(n, _, _)| *n));
        names.extend(["xrCreateInstance", "xrGetInstanceProcAddr", "xrInitializeLoaderKHR", "ovr_PopMessage"]);
        let imports: Vec<(&str, Binding)> = names.iter().map(|n| (*n, Binding::Strong)).collect();
        let (rt, t) = table_for(&imports, &["stdout"], &[]);
        let (page, end) = rt.stub_page();

        let data: BTreeSet<&str> = t.rows.iter().filter(|r| r.answer == Answer::Data).map(|r| r.symbol.as_str()).collect();
        let mut checked = BTreeMap::<Answer, usize>::new();
        for (lib, entries) in &t.libraries {
            for (name, addr) in entries {
                if data.contains(name.as_str()) {
                    continue;
                }
                let a = *addr as u64;
                assert!((page..end).contains(&a), "{lib}:{name} resolved to {a:#x}, outside the stub page");
                assert!(rt.stub_name(a).is_some(), "{lib}:{name} resolved to {a:#x}, which is no registered stub");
                let answer = t.rows.iter().find(|r| &r.symbol == name).map_or(Answer::Thunk, |r| r.answer);
                *checked.entry(answer).or_default() += 1;
            }
        }
        println!("function imports checked against the stub page, by answer: {checked:?}");
        for a in [Answer::Cordial, Answer::Host, Answer::Thunk, Answer::OpenXr, Answer::Stop] {
            assert!(checked.get(&a).copied().unwrap_or(0) > 0, "no {a:?} import was checked");
        }
    }

    /// Weak gcov hooks stay null as on Android; an omitted import and a data
    /// import with no known layout are not registered at all, which is what
    /// makes the link fail naming them.
    #[test]
    fn what_is_not_answered_is_not_registered() {
        let (_rt, t) = table_for(
            &[("__gcov_dump", Binding::Weak), ("strlen", Binding::Strong),
              ("some_mystery_variable", Binding::Strong), ("stdout", Binding::Strong)],
            &["some_mystery_variable", "stdout"], &["strlen"]);
        assert_eq!(address(&t, "__gcov_dump"), None);
        assert_eq!(address(&t, "strlen"), None);
        assert_eq!(address(&t, "some_mystery_variable"), None);
        let row = |n: &str| t.rows.iter().find(|r| r.symbol == n).unwrap().answer;
        assert_eq!(row("__gcov_dump"), Answer::WeakNull);
        assert_eq!(row("strlen"), Answer::Unanswered);
        assert_eq!(row("some_mystery_variable"), Answer::Unanswered);
        assert_eq!(row("stdout"), Answer::Data);
    }

    /// A name the hand-written thunks answer must not also be listed as a
    /// gap, or the import table would say it stops while it does not.
    #[test]
    fn no_thunk_is_also_a_named_gap() {
        let rt = Runtime::new(cordial_guest::Options::default());
        let mut names: Vec<&str> = Vec::new();
        for n in ["getauxval", "fopen", "uname", "dlopen", "dlsym", "dl_iterate_phdr", "open", "fstat", "mmap",
                  "vsnprintf", "sscanf", "syscall", "prctl", "newlocale", "uselocale", "strtold_l",
                  "iswalpha_l", "mbrtowc", "pthread_once", "__cxa_atexit", "bsearch"] {
            names.push(n);
        }
        for n in names {
            assert!(crate::guest_libc::handler(n, &rt, &|_| None).is_some(), "{n} has no thunk");
            assert_eq!(named_gap(n), None, "{n} is both a thunk and a named gap");
        }
    }

    #[test]
    fn nothing_described_is_also_a_named_gap() {
        for (n, _, _) in FUNCS {
            assert_eq!(named_gap(n), None, "{n} is both dispatched and named as a gap");
        }
    }
}
