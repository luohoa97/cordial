# Third-party notices

Cordial as a whole is GPL-3.0-or-later (see [`LICENSE`](LICENSE)). It
incorporates the components below, each under its own licence, and each of those
licences requires its notice to travel with the software — in source *and* in
binary form. This file exists so that obligation is met by anyone redistributing
Cordial, including from the Flatpak, which installs this file to
`/app/share/licenses/cordial/`.

MIT and Apache-2.0 are both compatible with GPL-3.0 in this direction: their
terms are satisfied while the combined work is offered under the GPL. Preserving
these notices is a condition of that, not a courtesy.

---

## libbadcpu — MIT

x86-64 CPU *feature* emulator. Vendored at
[`third_party/libbadcpu/`](third_party/libbadcpu), from
[`Z3ki/sober-oss`](https://github.com/Z3ki/sober-oss) at commit
`e48a905efdffa1ad49a3ebb873895bcff73aa935`. Cordial vendors only
`src/libbadcpu/`, `include/badcpu.h` and the test.

Upstream licence text is preserved verbatim at
[`third_party/libbadcpu/LICENSE.upstream`](third_party/libbadcpu/LICENSE.upstream).

```
MIT License

Copyright (c) 2026 Sober OSS Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## mcpelauncher-linker — MIT

The host wrapper around the AOSP bionic linker. Submodule at
[`third_party/mcpelauncher-linker/`](third_party/mcpelauncher-linker), from
[`minecraft-linux/mcpelauncher-linker`](https://github.com/minecraft-linux/mcpelauncher-linker).

```
MIT License

Copyright (c) 2024 ChristopherHX and MCMrARM
```

Full text: `third_party/mcpelauncher-linker/LICENSE`.

---

## Android Open Source Project (bionic) — Apache-2.0 and BSD

The bionic linker and headers carried inside `mcpelauncher-linker`. The NOTICE
file required by Apache-2.0 §4(d) is preserved at
`third_party/mcpelauncher-linker/core/NOTICE`.

```
Android Code
Copyright 2005-2008 The Android Open Source Project
```

Portions of bionic are under BSD licences from the original authors, whose
notices are preserved in the individual source files as required.

---

## libjnivm — MIT

The JNI virtual machine that stands in for Android's. Submodule at
[`third_party/libjnivm/`](third_party/libjnivm), from
[`ChristopherHX/libjnivm`](https://github.com/ChristopherHX/libjnivm).

```
MIT License

Copyright (c) 2019 ChristopherHX
```

Full text: `third_party/libjnivm/LICENSE`.

---

## mocktail — Apache-2.0

Both vendored and read, which is why this entry is longer than the others.

**Vendored.** [`third_party/mocktail-webview/`](third_party/mocktail-webview)
holds unmodified copies of mocktail's implementation of Roblox's web-view
protocol, kept as the reference for Cordial's own web window. They are not
compiled — nothing in the build reads that directory.

```
Copyright 2026 komaruworld

Licensed under the Apache License, Version 2.0
```

Full text: `third_party/mocktail-webview/LICENSE`.

**Derived and adapted.** Cordial's `crates/cordial-shell/src/webview_policy.rs`
is derived from mocktail's `webview_helper_policy.cc`;
`crates/cordial-runtime/src/permissions.rs` follows the discovery pattern in its
`roblox_permissions_bridge.cc`; and the performance tables in
`crates/cordial-shell/src/shell_config.rs` are adapted from its own. Those are
adaptations of Apache-2.0 work and are named as such in each file.

**Read, and the reason several things here are right rather than guessed.**
The field order of Roblox's `NativeTextBoxInfo` — including which slot carries
`textWrapped` and the `xAlign`/`yAlign` pair — came from mocktail's constructor
rather than from a stripped binary. So did several thread-count and pipeline
flag values, the platform identity string Cordial reports, and a number of
engine behaviours confirmed by watching mocktail run against the same build.
`docs/analysis/flag-init.md` cites it throughout.

This project reads mocktail deliberately and says so. The line it holds is that
ideas, call orders, field layouts and documented shapes may be taken with
credit, and implementations may not be transcribed — see
[CLAUDE.md](CLAUDE.md).

---

## Android Game Development Kit — Apache-2.0

Not vendored. AGDK's `GameActivity` source was **read** to get the
`onTouchEventNative` argument packing and the surface/lifecycle callback
contract right rather than guessing at them. No AGDK code is copied into this
repository; the reference is recorded here because it is the reason those
signatures are correct.

---

## dynarmic — 0BSD, and what it bundles

The arm64 dynamic recompiler behind `crates/cordial-guest`, compiled only into
x86-64 builds with the `vr` feature, which every package turns on. Submodule at [`third_party/dynarmic/`](third_party/dynarmic), from
[`azahar-emu/dynarmic`](https://github.com/azahar-emu/dynarmic) at commit
`a46601580d5512d324104f985b5f0209dc980ddc`.

```
Copyright (C) 2017 merryhime <git@mary.rs>

Permission to use, copy, modify, and/or distribute this software for
any purpose with or without fee is hereby granted.
```

Full text: `third_party/dynarmic/LICENSE.txt`. 0BSD asks for no notice; it is
listed so the libraries below have a place to hang from.

dynarmic's own submodules under `third_party/dynarmic/externals/` are compiled
into the same binary, each at the commit its submodule pins:

| Library | Licence | Full text |
|---|---|---|
| fmt | MIT, with an exception for object code | `externals/fmt/LICENSE` |
| mcl | MIT | `externals/mcl/LICENSE` |
| tsl-robin-map (header-only) | MIT | `externals/robin-map/LICENSE` |
| xbyak (header-only) | BSD-3-Clause | `externals/xbyak/COPYRIGHT` |
| Zydis | MIT | `externals/zydis/LICENSE` |
| Zycore | MIT | `externals/zycore/LICENSE` |

```
fmt:     Copyright (c) 2012 - present, Victor Zverovich and {fmt} contributors
mcl:     Copyright (c) 2022 merryhime
robin-map: Copyright (c) 2017 Thibaut Goetghebuer-Planchon <tessil@gmx.com>
xbyak:   Copyright (c) 2007 MITSUNARI Shigeo. All rights reserved.
Zydis:   Copyright (c) 2014-2024 Florian Bernd
Zycore:  Copyright (c) 2018-2024 Florian Bernd
```

The other three of dynarmic's submodules (biscuit, Catch2, oaknut) and Zydis's
own copy of Zycore are not compiled into Cordial.

Boost's headers (`boost::icl`, `boost::variant`) are also compiled in, under
the Boost Software License 1.0. They are not vendored: they come from the build
environment (`boost-devel`, `libboost-dev`, Arch's `boost`, Nix's `boost`; the
Flatpak copies Boost 1.83.0's headers in a module that ships nothing). BSL-1.0
does not require the notice to accompany object code.

Each package installs the seven licence files above beside Cordial's own, as
`dynarmic-0BSD.txt`, `fmt-MIT.txt`, `mcl-MIT.txt`, `robin-map-MIT.txt`,
`xbyak-BSD-3-Clause.txt`, `zydis-MIT.txt` and `zycore-MIT.txt`.

---

## Khronos registries and headers — Apache-2.0, or Apache-2.0 OR MIT

The guest's Vulkan, OpenXR and GL/EGL call tables are generated from Khronos's
machine-readable registries by `tools/vr/gen-guest-{vk,xr,gl}.py`, and the
output is committed and compiled into x86-64 builds with the `vr` feature. The registries themselves
are not vendored; each generated file names the release and commit it came
from.

| Generated file | From | Licence of the source |
|---|---|---|
| `crates/cordial-runtime/src/guest_vk_table.rs`, `guest_vk_probe.c` | `vk.xml`, Vulkan-Headers v1.4.341 | Apache-2.0 OR MIT |
| `crates/cordial-runtime/src/guest_xr_table.rs`, `guest_xr_probe.c` | `xr.xml`, OpenXR-SDK-Source release-1.1.47 | Apache-2.0 OR MIT |
| `crates/cordial-runtime/src/guest_gl_table.rs` | `gl.xml` (OpenGL-Registry) and `egl.xml` (EGL-Registry) | Apache-2.0 |

```
Copyright 2013-2026 The Khronos Group Inc.
SPDX-License-Identifier: Apache-2.0              (gl.xml, egl.xml)
SPDX-License-Identifier: Apache-2.0 OR MIT       (vk.xml, xr.xml)
```

Read from each file's own header at the commit named in the generated file.
The two `*_probe.c` files are compiled only by tests. The Apache-2.0 text
travels with every package as `mocktail-webview-Apache-2.0.txt`.

The Flatpak also bundles the OpenXR loader, built from OpenXR-SDK
release-1.1.47 (`libopenxr_loader.so.1`; loader sources Apache-2.0 OR MIT,
with its vendored jsoncpp, MIT), because neither of its runtimes ships one.
It installs `openxr-loader-Apache-2.0.txt` and `jsoncpp-MIT.txt` beside the
others. The other packages use the distribution's loader.

`third_party/openxr/include/openxr/` holds `openxr.h`, `openxr_platform.h` and
`openxr_platform_defines.h` from OpenXR-SDK release-1.1.47, unmodified,
Apache-2.0 OR MIT (each carries its SPDX line; `NOTICE` has the entry). They are
compiled only by the OpenXR layout test, not into any binary.

---

## What is *not* here

**Roblox.** Cordial contains no Roblox code, APK, asset or decompiled material,
and never will. It loads the official Android client that the user supplies from
their own installation. Roblox is a trademark of Roblox Corporation, which does
not endorse this project.
