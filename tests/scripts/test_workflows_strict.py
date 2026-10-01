#!/usr/bin/env python3
"""Every .github/workflows/*.yml parses with duplicate mapping keys rejected.

GitHub refuses a workflow with a duplicate key (the run fails with 0 jobs),
while PyYAML's default loader silently keeps the last one and the regex-based
contracts never notice. Needs PyYAML (preinstalled on ubuntu-latest).
"""
from pathlib import Path
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
CI = ROOT / ".github/workflows/ci.yml"


class StrictLoader(yaml.SafeLoader):
    pass


def _mapping(loader, node, deep=False):
    seen = set()
    for key_node, _ in node.value:
        key = loader.construct_object(key_node, deep=True)
        if key in seen:
            raise yaml.constructor.ConstructorError(
                None, None, f"duplicate key {key!r}", key_node.start_mark)
        seen.add(key)
    return yaml.SafeLoader.construct_mapping(loader, node, deep)


StrictLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _mapping)


def load_strict(text):
    return yaml.load(text, Loader=StrictLoader)


class StrictWorkflows(unittest.TestCase):
    def test_every_workflow_parses_without_duplicate_keys(self):
        files = sorted((ROOT / ".github/workflows").glob("*.y*ml"))
        self.assertTrue(files)
        for f in files:
            with self.subTest(workflow=f.name):
                self.assertIsInstance(load_strict(f.read_text()), dict)

    def test_a_duplicate_key_is_rejected(self):
        with self.assertRaises(yaml.YAMLError):
            load_strict("a:\n  if: x\n  with: {}\n  if: y\n")

    def test_a_duplicated_step_if_in_ci_is_rejected(self):
        text = CI.read_text()
        marker = "          save-if: false\n"
        mutated = text.replace(marker, marker + "        if: x\n        if: y\n", 1)
        self.assertNotEqual(text, mutated)
        with self.assertRaises(yaml.YAMLError):
            load_strict(mutated)

    def test_cache_warm_matrix_is_exactly_one_profile_dimension(self):
        strat = load_strict(CI.read_text())["jobs"]["cache-warm"]["strategy"]
        self.assertEqual(set(strat), {"fail-fast", "matrix"})
        self.assertEqual(strat["matrix"], {"profile": ["test", "release", "clippy"]})


if __name__ == "__main__":
    unittest.main()
