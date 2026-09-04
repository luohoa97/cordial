// A virtual pointer that stays alive, for driving Cordial's own widgets in a
// nested compositor.
//
// This exists because nothing else here can click. `cordial_click` goes to the
// engine's input entry points, one layer below the display server, so it can
// never reach a GTK widget -- which is most of what the text editor now is.
// `wlrctl pointer` binds zwlr_virtual_pointer_v1, sends the motion and exits
// immediately, and the device is destroyed before the compositor acts on it:
// measured under both cage and sway by compositing the cursor into a
// screenshot and diffing, zero pixels changed either time. sway's own
// `swaymsg seat - cursor set` does nothing for the same underlying reason --
// a headless seat has `capabilities: 0`, so there is no pointer to move until
// something provides one and keeps providing it.
//
// So: create the pointer, then block on stdin and keep it. One command per
// line, so a shell script can drive a whole interaction:
//
//     move <x> <y>            absolute, in output coordinates
//     down|up|click [button]  button is left (default), right or middle
//     quit
//
// **The button argument is not a convenience.** Left was the only one this
// sent, and left is the one button Cordial's pointer lock deliberately does
// *not* capture for -- a left drag is how every slider and scrollbar in
// Roblox's own interface is used. So the capture path could not be driven
// from here at all: the right-button camera drag, the lock it takes and the
// stale request it leaves behind were all untestable with the instrument
// that existed, which is why they shipped on reading alone.
//
// Prints "ok" per command so a caller can synchronise instead of sleeping.
#include <linux/input-event-codes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <wayland-client.h>
#include "wlr-virtual-pointer-unstable-v1-client-protocol.h"

static struct zwlr_virtual_pointer_manager_v1 *mgr;
static struct wl_seat *seat;
static struct zwlr_virtual_pointer_v1 *ptr;
static unsigned width = 1280, height = 720, clock_ms;

static void global(void *d, struct wl_registry *r, uint32_t name,
                   const char *iface, uint32_t ver) {
    (void)d; (void)ver;
    if (!strcmp(iface, zwlr_virtual_pointer_manager_v1_interface.name))
        mgr = wl_registry_bind(r, name, &zwlr_virtual_pointer_manager_v1_interface, 1);
    else if (!strcmp(iface, wl_seat_interface.name))
        seat = wl_registry_bind(r, name, &wl_seat_interface, 1);
}
static void global_remove(void *d, struct wl_registry *r, uint32_t n) { (void)d; (void)r; (void)n; }
static const struct wl_registry_listener reg_l = { global, global_remove };

int main(int argc, char **argv) {
    if (argc > 2) { width = atoi(argv[1]); height = atoi(argv[2]); }
    struct wl_display *dpy = wl_display_connect(NULL);
    if (!dpy) { fprintf(stderr, "no display\n"); return 1; }
    struct wl_registry *reg = wl_display_get_registry(dpy);
    wl_registry_add_listener(reg, &reg_l, NULL);
    wl_display_roundtrip(dpy);
    if (!mgr) { fprintf(stderr, "compositor offers no zwlr_virtual_pointer_manager_v1\n"); return 2; }
    ptr = zwlr_virtual_pointer_manager_v1_create_virtual_pointer(mgr, seat);
    wl_display_roundtrip(dpy);
    fprintf(stderr, "ready %ux%u\n", width, height);
    fflush(stderr);

    char line[256];
    while (fgets(line, sizeof line, stdin)) {
        unsigned x, y;
        char which[16] = "";
        clock_ms += 16;
        if (sscanf(line, "move %u %u", &x, &y) == 2) {
            zwlr_virtual_pointer_v1_motion_absolute(ptr, clock_ms, x, y, width, height);
            zwlr_virtual_pointer_v1_frame(ptr);
        } else if (!strncmp(line, "down", 4) || !strncmp(line, "up", 2)
                   || !strncmp(line, "click", 5)) {
            // An unknown name is refused rather than quietly taken as left.
            // A test that meant to drag with the right button and silently
            // dragged with the left would pass against the wrong gesture.
            const char *arg = strpbrk(line, " \t");
            unsigned button = BTN_LEFT;
            if (arg && sscanf(arg, "%15s", which) == 1 && *which) {
                if (!strcmp(which, "right")) button = BTN_RIGHT;
                else if (!strcmp(which, "middle")) button = BTN_MIDDLE;
                else if (strcmp(which, "left")) {
                    printf("err unknown button %s\n", which); fflush(stdout);
                    continue;
                }
            }
            if (strncmp(line, "up", 2) != 0) {
                zwlr_virtual_pointer_v1_button(ptr, clock_ms, button,
                                               WL_POINTER_BUTTON_STATE_PRESSED);
                zwlr_virtual_pointer_v1_frame(ptr);
            }
            if (strncmp(line, "down", 4) != 0) {
                clock_ms += 16;
                zwlr_virtual_pointer_v1_button(ptr, clock_ms, button,
                                               WL_POINTER_BUTTON_STATE_RELEASED);
                zwlr_virtual_pointer_v1_frame(ptr);
            }
        } else if (!strncmp(line, "quit", 4)) {
            break;
        }
        wl_display_flush(dpy);
        wl_display_roundtrip(dpy);
        printf("ok\n"); fflush(stdout);
    }
    return 0;
}
