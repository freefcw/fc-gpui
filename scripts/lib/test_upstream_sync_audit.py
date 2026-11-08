#!/usr/bin/env python3
"""Unit tests for the pure validation helpers in upstream_sync_audit.py."""

import sys
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import upstream_sync_audit as audit  # noqa: E402


def sha(digit: str) -> str:
    return digit * 40


class ValidateAuditTests(unittest.TestCase):
    def valid_audit(self):
        baseline = sha("a")
        audited = sha("b")
        return {
            "baseline": baseline,
            "audited_upstream": audited,
            "paths": ["crates/gpui/src"],
            "entries": [
                {
                    "commit": sha("c"),
                    "status": "equivalent",
                    "note": "covered by the local implementation",
                }
            ],
        }

    def test_valid_audit_is_normalized(self):
        result = audit.validate_audit(self.valid_audit())

        self.assertEqual(result[0], sha("a"))
        self.assertEqual(result[1], sha("b"))
        self.assertEqual(result[2], ["crates/gpui/src"])
        self.assertEqual(result[5], [sha("c")])
        self.assertEqual(result[6], [])

    def test_duplicate_entries_are_rejected(self):
        value = self.valid_audit()
        value["entries"].append(value["entries"][0].copy())

        with self.assertRaises(SystemExit):
            audit.validate_audit(value)

    def test_invalid_status_is_rejected(self):
        value = self.valid_audit()
        value["entries"][0]["status"] = "unknown"

        with self.assertRaises(SystemExit):
            audit.validate_audit(value)

    def test_backport_entries_are_collected(self):
        value = self.valid_audit()
        value["entries"][0]["status"] = "backport"

        result = audit.validate_audit(value)

        self.assertEqual(result[6], [sha("c")])


if __name__ == "__main__":
    unittest.main()
