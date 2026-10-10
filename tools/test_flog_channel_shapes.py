#!/usr/bin/env python3
"""End-to-end tests for the observed FLog value-shape lookup (#13)."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("flog_channel_shapes.py")


class FLogChannelShapesTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.settings = Path(self.tmp.name, "settings.json")
        self.settings.write_text(json.dumps({"applicationSettings": {
            "FLogNetwork": "7",
            "FLogAudio": "Info",
            "DFLogWebSocketTraceError": "Warning,6",
            "FLogOddChannel": "not-a-recognised-format",
            "DFLogFiltered_PlaceFilter": "Verbose,9;123;456",
            "FFlagUnrelated": "True",
        }}))

    def run_lookup(self, *args):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--settings", str(self.settings), *args],
            capture_output=True, text=True,
        )

    def test_lookup_uses_numeric_and_severity_values_from_observed_document(self):
        result = self.run_lookup("--channel", "FLogNetwork", "--channel", "FLogAudio",
                                 "--channel", "DFLogWebSocketTraceError")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("FLogNetwork\tnumber\t7", result.stdout)
        self.assertIn("FLogAudio\tseverity\tInfo", result.stdout)
        self.assertIn("DFLogWebSocketTraceError\tseverity\tWarning,6", result.stdout)
        self.assertNotIn("FFlagUnrelated", result.stdout)

    def test_unseen_and_nonstandard_values_are_not_guessed(self):
        result = self.run_lookup("--channel", "FLogNeverObserved",
                                 "--channel", "FLogOddChannel",
                                 "--channel", "DFLogFiltered_PlaceFilter")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("FLogNeverObserved\tunknown\t-", result.stdout)
        self.assertIn("FLogOddChannel\tunknown\tnot-a-recognised-format", result.stdout)
        self.assertIn("DFLogFiltered_PlaceFilter\tseverity-filtered\tVerbose,9;123;456", result.stdout)

    def test_wrong_override_shape_warns_without_guessing_unseen_channels(self):
        flags = Path(self.tmp.name, "flags.json")
        flags.write_text(json.dumps({
            "FLogAudio": "100",
            "FLogNetwork": "9",
            "DFLogWebSocketTraceError": "Warning,6",
            "FLogNeverObserved": "100",
        }))
        result = self.run_lookup("--flags", str(flags))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("FLogAudio: override '100' has number syntax", result.stderr)
        self.assertIn("observed severity syntax", result.stderr)
        self.assertNotIn("FLogNetwork:", result.stderr)
        self.assertNotIn("FLogNeverObserved:", result.stderr)

    def test_accepts_unwrapped_cached_document(self):
        self.settings.write_text(json.dumps({"FLogNetwork": "7"}))
        result = self.run_lookup("--channel", "FLogNetwork")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("FLogNetwork\tnumber\t7", result.stdout)


if __name__ == "__main__":
    unittest.main()
