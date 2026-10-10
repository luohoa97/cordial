#!/usr/bin/env python3
"""Inspect FLog/DFLog value shapes observed in a Roblox settings document.

Issue #13: a numeric value can silence a channel that expects a severity
name. This is evidence from a *particular settings document*, not a complete
catalogue of declarations in Roblox's binary. Unknown channels remain unknown.

Typical use:
    python3 tools/flog_channel_shapes.py --settings ~/.cache/cordial/clientsettings.json
    python3 tools/flog_channel_shapes.py --settings /tmp/clientsettings.json \
        --channel FLogAudio --flags ~/.config/cordial/flags.json
"""

import argparse
import json
from pathlib import Path
import re
import sys


NUMBER = re.compile(r"[+-]?\d+\Z")
SEVERITY = re.compile(r"(?:Fatal|Error|Warning|Info|Verbose|Debug|Trace)(?:,\d+)?\Z")


def value_shape(value: str, channel: str = "") -> str:
    """Report syntax observed in this document; never infer missing values."""
    if not isinstance(value, str):
        value = str(value)
    sample = value
    filtered = channel.endswith("_PlaceFilter") and ";" in sample
    if filtered:
        sample = sample.split(";", 1)[0]
    if NUMBER.fullmatch(sample):
        shape = "number"
    elif SEVERITY.fullmatch(sample):
        shape = "severity"
    else:
        return "unknown"
    return f"{shape}-filtered" if filtered else shape


def load_object(path: Path) -> dict:
    data = json.loads(path.read_text())
    if not isinstance(data, dict):
        raise ValueError(f"{path}: expected a JSON object")
    if "applicationSettings" in data:
        data = data["applicationSettings"]
        if not isinstance(data, dict):
            raise ValueError(f"{path}: applicationSettings is not a JSON object")
    return data


def observed_text(value: object) -> str:
    return value if isinstance(value, str) else json.dumps(value)


def check_overrides(settings: dict, overrides: dict) -> list[str]:
    warnings = []
    for key, proposed in sorted(overrides.items()):
        if not key.startswith(("FLog", "DFLog")) or key not in settings:
            continue
        expected = value_shape(observed_text(settings[key]), key)
        supplied = value_shape(observed_text(proposed), key)
        # A place-filter suffix has additional semantics beyond its prefix;
        # reject no value based on incomplete evidence of its syntax.
        if expected not in ("number", "severity"):
            continue
        if supplied != expected:
            warnings.append(
                f"{key}: override {observed_text(proposed)!r} has {supplied} syntax; "
                f"observed {expected} syntax in this settings document "
                "(confirm on a running client)"
            )
    return warnings


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--settings", type=Path,
        default=Path.home() / ".cache/cordial/clientsettings.json",
        help="cached Roblox settings JSON or applicationSettings JSON from the CDN",
    )
    parser.add_argument("--channel", action="append", default=[],
                        help="lookup one channel; may be repeated")
    parser.add_argument("--flags", type=Path,
                        help="optional flags.json to compare observed value syntax")
    args = parser.parse_args(argv)

    try:
        settings = load_object(args.settings)
        overrides = load_object(args.flags) if args.flags else {}
    except (OSError, ValueError) as exc:
        parser.error(str(exc))

    channels = args.channel or sorted(
        key for key in settings if key.startswith(("FLog", "DFLog"))
    )
    print("channel\tobserved_shape\tobserved_value")
    for channel in channels:
        if channel in settings:
            value = observed_text(settings[channel])
            print(f"{channel}\t{value_shape(value, channel)}\t{value}")
        else:
            print(f"{channel}\tunknown\t-")

    for message in check_overrides(settings, overrides):
        print(f"warning: {message}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
