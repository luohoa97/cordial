# Compiler caches for the native subtree: ccache if it is installed, else
# sccache, else nothing.
#
# Both tools take the same `cache compiler args...` shape, and CMake carries
# CMAKE_<LANG>_COMPILER_LAUNCHER into every target below the directory that
# sets it -- the bionic linker and dynarmic included -- so this is one file
# rather than a flag repeated on each add_subdirectory.
#
# Neither tool is a build requirement. When neither is on PATH the launchers
# are set to the empty string and the compile lines are exactly what they were
# before this file existed. That matters for the Flatpak, the Nix build and
# every distro package, none of which install a cache; and it is why the
# launcher is set unconditionally instead of only when found: a CMakeCache.txt
# left by a configure that had a cache must not keep naming a tool that has
# since been removed.
#
# ccache first where both exist. On a local disk it is the faster of the two
# for C and C++, and sccache is already the Rust wrapper
# (.cargo/config.toml), so the two caches divide by language rather than race
# for the same translation units.
#
# CORDIAL_NATIVE_CACHE names a launcher explicitly (an absolute path, or a
# sccache with a remote backend behind it); CORDIAL_COMPILER_CACHE=off turns
# caching off without changing the compiler, which is the control arm of the
# build measurement in docs/adr/ADR-067.

set(_cordial_native_cache "")

if(DEFINED ENV{CORDIAL_NATIVE_CACHE} AND NOT "$ENV{CORDIAL_NATIVE_CACHE}" STREQUAL "")
  set(_cordial_native_cache "$ENV{CORDIAL_NATIVE_CACHE}")
elseif(NOT DEFINED ENV{CORDIAL_COMPILER_CACHE} OR NOT "$ENV{CORDIAL_COMPILER_CACHE}" STREQUAL "off")
  # Copied out of the cache before it is cleared: find_program() caches its
  # answer, and a cached absolute path is exactly the stale launcher the
  # comment above says this must not keep.
  find_program(_cordial_ccache ccache)
  find_program(_cordial_sccache sccache)
  if(_cordial_ccache)
    set(_cordial_native_cache "${_cordial_ccache}")
  elseif(_cordial_sccache)
    set(_cordial_native_cache "${_cordial_sccache}")
  endif()
  unset(_cordial_ccache CACHE)
  unset(_cordial_sccache CACHE)
endif()

set(CMAKE_C_COMPILER_LAUNCHER "${_cordial_native_cache}")
set(CMAKE_CXX_COMPILER_LAUNCHER "${_cordial_native_cache}")

if(_cordial_native_cache)
  message(STATUS "cordial: native compiler cache: ${_cordial_native_cache}")
else()
  message(STATUS "cordial: native compiler cache: none")
endif()
