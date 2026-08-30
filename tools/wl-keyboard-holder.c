// A virtual keyboard that stays alive, for typing real keys at Cordial in a
// nested compositor.
//
// This is the keyboard half of `wl-pointer-holder.c` and it exists for two
// reasons, one of which was learned the expensive way.
//
// The first is the same as the pointer's: a device that is created and
// destroyed in the same breath is not a device. `wlrctl keyboard type` binds
// zwp_virtual_keyboard_manager_v1, sends the key and exits, and whether the
// compositor got there first is a race. `docs/NEXT.md` recorded four wlrctl
// calls that did land, which reads as proof that it works; it is proof that it
// worked four times.
//
// The second reason is the one that matters. **A headless seat with no input
// device advertises `capabilities: 0`, and Cordial reads the seat's
// capabilities once, at `open()`.** So under a compositor whose seat never had
// a keyboard, Cordial never binds its own `wl_keyboard` -- and Cordial's key
// path, including the guard that stops GDK and Cordial both inserting the same
// character, never runs at all. Every text-entry result taken that way was
// taken with half the code under test switched off, which is why `NEXT.md`
// labelled the double-insert guard `INFERRED` rather than measured.
//
// Holding the keyboard open from before the client starts until after it exits
// makes the seat look like a real one for the whole run, so the guard is
// exercised. If it is wrong, every character appears twice and the assertions
// in `tools/text-input-e2e.sh` say so.
//
// One command per line; prints "ok" per command so a caller can synchronise
// instead of sleeping:
//
//     type <string>          each character, in order
//     key [mod+]<keysym>     e.g. `key BackSpace`, `key ctrl+a`, `key shift+Left`
//                            mods: ctrl, shift, alt, super -- combine with '+'
//     down [mod+]<keysym>    press only, held until `up`. One pending key at a
//                            time; a second `down` before `up` overwrites it.
//     up                     release whatever `down` pressed. The gap between
//                            them is the caller's, not this process's --
//                            unlike `key`, which hardcodes 12ms.
//     quit
//
// Build (in the container, which is where sway and wlrctl live):
//     tools/build-wl-holders.sh
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>
#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>
#include "virtual-keyboard-unstable-v1-client-protocol.h"

static struct zwp_virtual_keyboard_manager_v1 *mgr;
static struct wl_seat *seat;
static struct zwp_virtual_keyboard_v1 *kbd;
static struct xkb_keymap *keymap;
static struct wl_display *dpy;
static uint32_t clock_ms;
static xkb_mod_index_t mod_shift, mod_ctrl, mod_alt, mod_super;

static void global(void *d, struct wl_registry *r, uint32_t name,
                   const char *iface, uint32_t ver) {
    (void)d; (void)ver;
    if (!strcmp(iface, zwp_virtual_keyboard_manager_v1_interface.name))
        mgr = wl_registry_bind(r, name, &zwp_virtual_keyboard_manager_v1_interface, 1);
    else if (!strcmp(iface, wl_seat_interface.name))
        seat = wl_registry_bind(r, name, &wl_seat_interface, 1);
}
static void global_remove(void *d, struct wl_registry *r, uint32_t n) { (void)d; (void)r; (void)n; }
static const struct wl_registry_listener reg_l = { global, global_remove };

// Find a keycode that produces `sym`, and the shift level it needs. Scanning
// the keymap rather than hardcoding a table is what makes this independent of
// the layout the keymap was built from.
static int lookup(xkb_keysym_t sym, uint32_t *keycode, int *needs_shift) {
    xkb_keycode_t min = xkb_keymap_min_keycode(keymap);
    xkb_keycode_t max = xkb_keymap_max_keycode(keymap);
    for (xkb_keycode_t kc = min; kc <= max; kc++) {
        int levels = xkb_keymap_num_levels_for_key(keymap, kc, 0);
        for (int lvl = 0; lvl < levels; lvl++) {
            const xkb_keysym_t *syms;
            int n = xkb_keymap_key_get_syms_by_level(keymap, kc, 0, lvl, &syms);
            for (int i = 0; i < n; i++) {
                if (syms[i] != sym) continue;
                *keycode = kc - 8;      // evdev, not xkb
                *needs_shift = lvl > 0;
                return 1;
            }
        }
    }
    return 0;
}

static void send_mods(uint32_t mask) {
    zwp_virtual_keyboard_v1_modifiers(kbd, mask, 0, 0, 0);
}

// A single pending press, for `down`/`up`, which exist to put a controllable
// gap between a key's down and its up -- `press()` below hardcodes 12ms and
// that is not the thing under test. Added to verify commit 22127ba (pairing a
// forwarded press's release regardless of a focus change landing in between):
// the caller sleeps between `down` and `up` in the harness rather than in this
// process, so the gap is whatever the harness asks for, not a compiled-in one.
static uint32_t pending_kc;
static uint32_t pending_mask;
static int has_pending;

static int press(xkb_keysym_t sym, uint32_t extra_mods) {
    uint32_t kc; int shift = 0;
    if (!lookup(sym, &kc, &shift)) return 0;
    uint32_t mask = extra_mods | (shift ? (1u << mod_shift) : 0);
    if (mask) send_mods(mask);
    clock_ms += 12;
    zwp_virtual_keyboard_v1_key(kbd, clock_ms, kc, WL_KEYBOARD_KEY_STATE_PRESSED);
    clock_ms += 12;
    zwp_virtual_keyboard_v1_key(kbd, clock_ms, kc, WL_KEYBOARD_KEY_STATE_RELEASED);
    if (mask) send_mods(0);
    wl_display_flush(dpy);
    wl_display_roundtrip(dpy);
    // Real typing is not instantaneous and GTK's key handling is not
    // synchronous with the protocol; without a gap the compositor coalesces
    // and the widget sees fewer characters than were sent.
    usleep(30000);
    return 1;
}

static int key_down(xkb_keysym_t sym, uint32_t extra_mods) {
    uint32_t kc; int shift = 0;
    if (!lookup(sym, &kc, &shift)) return 0;
    uint32_t mask = extra_mods | (shift ? (1u << mod_shift) : 0);
    if (mask) send_mods(mask);
    clock_ms += 12;
    zwp_virtual_keyboard_v1_key(kbd, clock_ms, kc, WL_KEYBOARD_KEY_STATE_PRESSED);
    wl_display_flush(dpy);
    wl_display_roundtrip(dpy);
    pending_kc = kc;
    pending_mask = mask;
    has_pending = 1;
    return 1;
}

static int key_up(void) {
    if (!has_pending) return 0;
    clock_ms += 12;
    zwp_virtual_keyboard_v1_key(kbd, clock_ms, pending_kc, WL_KEYBOARD_KEY_STATE_RELEASED);
    if (pending_mask) send_mods(0);
    wl_display_flush(dpy);
    wl_display_roundtrip(dpy);
    has_pending = 0;
    return 1;
}

// Minimal UTF-8 decode. `type` is fed test strings, not arbitrary input.
static const char *next_cp(const char *s, uint32_t *cp) {
    unsigned char c = (unsigned char)*s;
    if (c < 0x80) { *cp = c; return s + 1; }
    if ((c & 0xe0) == 0xc0) { *cp = ((c & 0x1fu) << 6) | (s[1] & 0x3fu); return s + 2; }
    if ((c & 0xf0) == 0xe0) { *cp = ((c & 0x0fu) << 12) | ((s[1] & 0x3fu) << 6) | (s[2] & 0x3fu); return s + 3; }
    *cp = ((c & 0x07u) << 18) | ((s[1] & 0x3fu) << 12) | ((s[2] & 0x3fu) << 6) | (s[3] & 0x3fu);
    return s + 4;
}

int main(void) {
    dpy = wl_display_connect(NULL);
    if (!dpy) { fprintf(stderr, "no display\n"); return 1; }
    struct wl_registry *reg = wl_display_get_registry(dpy);
    wl_registry_add_listener(reg, &reg_l, NULL);
    wl_display_roundtrip(dpy);
    if (!mgr) { fprintf(stderr, "compositor offers no zwp_virtual_keyboard_manager_v1\n"); return 2; }

    struct xkb_context *ctx = xkb_context_new(XKB_CONTEXT_NO_FLAGS);
    struct xkb_rule_names names = { .rules = NULL, .model = "pc105",
                                    .layout = "us", .variant = NULL, .options = NULL };
    keymap = xkb_keymap_new_from_names(ctx, &names, XKB_KEYMAP_COMPILE_NO_FLAGS);
    if (!keymap) { fprintf(stderr, "no keymap\n"); return 3; }
    mod_shift = xkb_keymap_mod_get_index(keymap, XKB_MOD_NAME_SHIFT);
    mod_ctrl  = xkb_keymap_mod_get_index(keymap, XKB_MOD_NAME_CTRL);
    mod_alt   = xkb_keymap_mod_get_index(keymap, XKB_MOD_NAME_ALT);
    mod_super = xkb_keymap_mod_get_index(keymap, XKB_MOD_NAME_LOGO);

    char *ks = xkb_keymap_get_as_string(keymap, XKB_KEYMAP_FORMAT_TEXT_V1);
    size_t ks_len = strlen(ks) + 1;
    int fd = memfd_create("keymap", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, ks_len) < 0) { perror("memfd"); return 4; }
    void *map = mmap(NULL, ks_len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    memcpy(map, ks, ks_len);
    munmap(map, ks_len);

    kbd = zwp_virtual_keyboard_manager_v1_create_virtual_keyboard(mgr, seat);
    zwp_virtual_keyboard_v1_keymap(kbd, WL_KEYBOARD_KEYMAP_FORMAT_XKB_V1, fd, ks_len);
    close(fd);
    wl_display_roundtrip(dpy);
    fprintf(stderr, "ready\n");
    fflush(stderr);

    char line[1024];
    while (fgets(line, sizeof line, stdin)) {
        line[strcspn(line, "\n")] = 0;
        int ok = 1;
        if (!strncmp(line, "type ", 5)) {
            for (const char *p = line + 5; *p; ) {
                uint32_t cp; p = next_cp(p, &cp);
                if (!press(xkb_utf32_to_keysym(cp), 0)) {
                    fprintf(stderr, "no key for U+%04X\n", cp);
                    ok = 0;
                }
            }
        } else if (!strncmp(line, "key ", 4)) {
            char *spec = line + 4;
            uint32_t mods = 0;
            char *plus;
            while ((plus = strchr(spec, '+')) != NULL) {
                *plus = 0;
                if (!strcmp(spec, "ctrl")) mods |= 1u << mod_ctrl;
                else if (!strcmp(spec, "shift")) mods |= 1u << mod_shift;
                else if (!strcmp(spec, "alt")) mods |= 1u << mod_alt;
                else if (!strcmp(spec, "super")) mods |= 1u << mod_super;
                else { fprintf(stderr, "unknown modifier %s\n", spec); ok = 0; }
                spec = plus + 1;
            }
            xkb_keysym_t sym = xkb_keysym_from_name(spec, XKB_KEYSYM_NO_FLAGS);
            if (sym == XKB_KEY_NoSymbol) { fprintf(stderr, "unknown keysym %s\n", spec); ok = 0; }
            else if (!press(sym, mods)) { fprintf(stderr, "no key for %s\n", spec); ok = 0; }
        } else if (!strncmp(line, "down ", 5)) {
            char *spec = line + 5;
            uint32_t mods = 0;
            char *plus;
            while ((plus = strchr(spec, '+')) != NULL) {
                *plus = 0;
                if (!strcmp(spec, "ctrl")) mods |= 1u << mod_ctrl;
                else if (!strcmp(spec, "shift")) mods |= 1u << mod_shift;
                else if (!strcmp(spec, "alt")) mods |= 1u << mod_alt;
                else if (!strcmp(spec, "super")) mods |= 1u << mod_super;
                else { fprintf(stderr, "unknown modifier %s\n", spec); ok = 0; }
                spec = plus + 1;
            }
            xkb_keysym_t sym = xkb_keysym_from_name(spec, XKB_KEYSYM_NO_FLAGS);
            if (sym == XKB_KEY_NoSymbol) { fprintf(stderr, "unknown keysym %s\n", spec); ok = 0; }
            else if (!key_down(sym, mods)) { fprintf(stderr, "no key for %s\n", spec); ok = 0; }
        } else if (!strncmp(line, "up", 2)) {
            if (!key_up()) { fprintf(stderr, "no pending key to release\n"); ok = 0; }
        } else if (!strncmp(line, "quit", 4)) {
            break;
        }
        printf(ok ? "ok\n" : "err\n");
        fflush(stdout);
    }
    return 0;
}
