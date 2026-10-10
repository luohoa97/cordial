#!/usr/bin/env python3
"""Source-level guard for the Android InitParams base URL contract (#2).

This compares Cordial's supplied value to the engine log from the same APK on
Android. It does not replace a live check of the client's HTTP requests.
"""

import gzip
from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[1]
INIT_PARAMS = ROOT / "native" / "init_params.cpp"
ANDROID_TRACE = ROOT / "docs" / "traces" / "waydroid-roblox-startup.log.gz"


class InitParamsBaseUrlTest(unittest.TestCase):
    def test_base_url_matches_observed_android_client(self):
        with gzip.open(ANDROID_TRACE, "rt", errors="replace") as trace:
            observed = set(re.findall(r"The base url is (https://[^\s]+)", trace.read()))
        self.assertEqual(observed, {"https://www.roblox.com/"})

        source = INIT_PARAMS.read_text()
        supplied = re.search(r'p->baseURL\s*=\s*S\("([^"]+)"\);', source)
        self.assertIsNotNone(supplied, "InitParams::Create must supply a base URL")
        self.assertEqual(supplied.group(1), next(iter(observed)))


if __name__ == "__main__":
    unittest.main()
