# dynarmic and the C ABI shim over it, for crates/cordial-guest/build.rs.
#
# Only ever configured for an x86-64 host. dynarmic's A64 frontend has no
# arm64 host backend (its CMake stops with "Unimplemented frontend for this
# host architecture"), so the aarch64 build of this tree must not reach here;
# build.rs is where that is enforced.
cmake_minimum_required(VERSION 3.16)
project(cordial_guest LANGUAGES C CXX ASM)

# ccache/sccache launchers for dynarmic and the shim. Neither tool is
# required; see the file for why it is set either way.
include(${CMAKE_CURRENT_SOURCE_DIR}/../../../cmake/CompilerCache.cmake)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# Every static archive in one directory, so build.rs names one search path
# instead of guessing where each external's own CMake puts its output.
set(CMAKE_ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)

# The A64 frontend only: the Quest engine is arm64 throughout, and building
# A32 as well roughly doubles the compile for code nothing here would call.
set(DYNARMIC_FRONTENDS "A64" CACHE STRING "" FORCE)
set(DYNARMIC_TESTS OFF CACHE BOOL "" FORCE)
# Submodules rather than whatever fmt or Zydis the sysroot happens to have, so
# the recorded commit is the whole of what is compiled. Boost is the exception:
# dynarmic find_package()s it unconditionally, and its headers come from the
# sysroot (tools/vr/Dockerfile.sysroot).
set(DYNARMIC_USE_BUNDLED_EXTERNALS ON CACHE BOOL "" FORCE)
# A warning from a newer Clang in third-party code is not a reason for this
# build to stop.
set(DYNARMIC_WARNINGS_AS_ERRORS OFF CACHE BOOL "" FORCE)
# The precompiled header is a build-speed feature that has broken builds under
# a changed compiler before; nothing here needs it.
set(DYNARMIC_USE_PRECOMPILED_HEADERS OFF CACHE BOOL "" FORCE)

add_subdirectory(${CORDIAL_DYNARMIC_DIR} dynarmic EXCLUDE_FROM_ALL)

add_library(cordial_guest_shim STATIC shim.cpp host_call_x86_64.S)
target_link_libraries(cordial_guest_shim PRIVATE dynarmic)
# cmpxchg16b, for the 128-bit exclusive store callback.
target_compile_options(cordial_guest_shim PRIVATE -mcx16)
