"""Unit tests for tools/headless_compositor.py's pure decisions.

The parts that start a compositor are measured against real compositors in
`docs/analysis/headless-compositors.md`; these are the parts that can be
checked on a machine with no display at all. The selection rule matters most:
a harness that asks for virtual input must never be handed KWin just because
KWin is installed.
"""

import os
import pathlib
import struct
import sys
import unittest
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))

import headless_compositor as hc  # noqa: E402


class FakeExecution:
    """Just enough Execution for the selector: it only asks `which`."""

    box = None

    def __init__(self, present):
        self.present = set(present)

    def which(self, name):
        return f"/usr/bin/{name}" if name in self.present else None


def a_session(out_dir="/tmp/headless-test", app_id="Cordial"):
    return hc.Session(hc.BACKENDS["sway"], hc.Execution(None), out_dir=out_dir,
                      runtime_dir=f"{out_dir}/runtime", width=1280, height=800,
                      display="wayland-1", log_path=f"{out_dir}/sway.log",
                      pid=None, app_id=app_id)


class BackendSelection(unittest.TestCase):
    def test_kwin_is_picked_when_it_is_the_only_compositor(self):
        chosen = hc._choose_backend(FakeExecution({"kwin_wayland"}), None,
                                    (hc.LAUNCH,))
        self.assertEqual(chosen, "kwin")

    def test_a_virtual_input_test_is_never_handed_kwin(self):
        chosen = hc._choose_backend(
            FakeExecution({"kwin_wayland", "sway"}), None,
            (hc.LAUNCH, hc.VIRTUAL_INPUT))
        self.assertEqual(chosen, "sway")

    def test_an_explicit_backend_that_cannot_do_the_job_says_which_part(self):
        with self.assertRaises(SystemExit) as caught:
            hc._choose_backend(FakeExecution({"kwin_wayland"}), "kwin",
                               (hc.LAUNCH, hc.VIRTUAL_INPUT))
        message = str(caught.exception)
        self.assertIn("virtual_input", message)
        self.assertIn("sway", message)

    def test_an_explicit_backend_that_is_not_installed_is_not_silently_replaced(self):
        with self.assertRaises(SystemExit) as caught:
            hc._choose_backend(FakeExecution({"kwin_wayland"}), "weston",
                               (hc.LAUNCH,))
        self.assertIn("weston", str(caught.exception))

    def test_unknown_names_are_rejected(self):
        with self.assertRaises(SystemExit):
            hc._choose_backend(FakeExecution({"sway"}), "cage", (hc.LAUNCH,))

    def test_the_capability_lists_match_what_each_one_is_used_for(self):
        self.assertTrue(hc.BACKENDS["sway"].has(hc.VIRTUAL_INPUT))
        self.assertFalse(hc.BACKENDS["kwin"].has(hc.VIRTUAL_INPUT))
        self.assertTrue(hc.BACKENDS["kwin"].has(hc.SCREENSHOT))
        self.assertTrue(hc.BACKENDS["kwin"].has(hc.IPC))
        self.assertFalse(hc.BACKENDS["weston"].has(hc.SCREENSHOT))
        self.assertTrue(hc.BACKENDS["mutter"].has(hc.LAUNCH))
        self.assertNotIn("cage", hc.BACKENDS)


class Environment(unittest.TestCase):
    def test_the_compositor_gets_no_wayland_display_to_nest_into(self):
        old = os.environ.get("WAYLAND_DISPLAY")
        os.environ["WAYLAND_DISPLAY"] = "wayland-somebody-else"
        try:
            session = a_session()
            self.assertNotIn("WAYLAND_DISPLAY", session.compositor_env())
            self.assertEqual(session.host_env()["WAYLAND_DISPLAY"], "wayland-1")
        finally:
            if old is None:
                os.environ.pop("WAYLAND_DISPLAY", None)
            else:
                os.environ["WAYLAND_DISPLAY"] = old

    def test_a_client_never_inherits_display(self):
        old = os.environ.get("DISPLAY")
        os.environ["DISPLAY"] = ":0"
        try:
            self.assertNotIn("DISPLAY", a_session().host_env())
        finally:
            if old is None:
                os.environ.pop("DISPLAY", None)
            else:
                os.environ["DISPLAY"] = old

    def test_box_execution_wraps_and_direct_does_not(self):
        self.assertEqual(hc.Execution(None).wrap(["sway"]), ["sway"])
        self.assertEqual(hc.Execution("cordial").wrap(["sway"]),
                         ["distrobox", "enter", "cordial", "--", "sway"])


class NeutralReadings(unittest.TestCase):
    def test_kwins_virtual_output_is_the_window_the_harness_asked_for(self):
        session = hc.Session(hc.BACKENDS["kwin"], hc.Execution(None),
                            out_dir="/tmp/headless-test",
                            runtime_dir="/tmp/headless-test/runtime",
                            width=1024, height=768, display="wayland-h",
                            log_path="/tmp/headless-test/kwin.log", pid=None)
        self.assertEqual(session.output_rect(), (0, 0, 1024, 768))

    def test_a_window_carries_its_surface_rectangle_separately_from_its_frame(self):
        w = hc.Window("1", "game", "Cordial", True, False, (10, 20, 800, 600),
                      pid=42, client_rect=(10, 66, 800, 554))
        self.assertEqual(w.rect, (10, 20, 800, 600))
        self.assertEqual(w.client_rect, (10, 66, 800, 554))


class SwayConfig(unittest.TestCase):
    def test_the_output_is_the_requested_size(self):
        text = hc.sway_config(a_session(), "", False)
        self.assertIn("output HEADLESS-1 mode 1280x800", text)
        self.assertNotIn("no_focus", text)

    def test_no_focus_names_the_client_it_must_not_focus(self):
        text = hc.sway_config(a_session(app_id="Cordial"), "", True)
        self.assertIn('no_focus [app_id="Cordial"]', text)

    def test_the_display_is_reported_from_inside_the_compositor(self):
        # Sway chooses its own socket name; a guessed one is what cost two
        # runs before the stamp existed.
        text = hc.sway_config(a_session("/tmp/has space"), "", False)
        self.assertIn("'/tmp/has space/sway-display'", text)
        self.assertIn("$WAYLAND_DISPLAY", text)


class Png(unittest.TestCase):
    def _pixels(self, path):
        data = pathlib.Path(path).read_bytes()
        self.assertEqual(data[:8], b"\x89PNG\r\n\x1a\n")
        pos, chunks = 8, {}
        while pos < len(data):
            (length,) = struct.unpack(">I", data[pos:pos + 4])
            tag = data[pos + 4:pos + 8]
            payload = data[pos + 8:pos + 8 + length]
            chunks[tag] = payload
            pos += 12 + length
        ihdr = chunks[b"IHDR"]
        width, height = struct.unpack(">II", ihdr[:8])
        raw = zlib.decompress(chunks[b"IDAT"])
        rows = []
        stride = width * 3 + 1
        for y in range(height):
            row = raw[y * stride:(y + 1) * stride]
            self.assertEqual(row[0], 0, "PNG filter must be none")
            rows.append([tuple(row[1 + x * 3:4 + x * 3]) for x in range(width)])
        return width, height, rows

    def test_kwins_rgbx_is_written_as_rgb_not_bgr(self):
        # KWin 6.8.2 writes R,G,B,X despite reporting QImage format 16. The
        # first version followed Qt's documented byte order and produced a
        # screenshot with red and blue swapped; this pins the measured order.
        path = "/tmp/headless-png-test.png"
        hc._write_png(path, 1, 1, 4, 16, b"\x11\x22\x33\xff")
        width, height, rows = self._pixels(path)
        os.unlink(path)
        self.assertEqual((width, height), (1, 1))
        self.assertEqual(rows, [[(0x11, 0x22, 0x33)]])

    def test_ppm_keeps_the_same_byte_order(self):
        path = "/tmp/headless-ppm-test.ppm"
        hc._write_ppm(path, 1, 1, 4, 16, b"\x11\x22\x33\xff")
        data = pathlib.Path(path).read_bytes()
        os.unlink(path)
        self.assertEqual(data, b"P6\n1 1\n255\n\x11\x22\x33")

    def test_a_truncated_capture_is_an_error_not_a_short_image(self):
        with self.assertRaises(RuntimeError):
            hc._write_png("/tmp/headless-png-bad.png", 2, 2, 8, 16, b"\x00" * 8)

    def test_an_unknown_format_is_refused(self):
        with self.assertRaises(RuntimeError):
            hc._write_png("/tmp/headless-png-bad.png", 1, 1, 4, 99, b"\x00" * 4)


if __name__ == "__main__":
    unittest.main()
