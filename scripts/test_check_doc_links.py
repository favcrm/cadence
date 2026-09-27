"""Regression cases for fresh-clone documentation path resolution."""

import importlib.util
from pathlib import Path
import unittest


SPEC = importlib.util.spec_from_file_location("doc_links", Path(__file__).with_name("check-doc-links.py"))
LINKS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LINKS)


class DocumentLinks(unittest.TestCase):
    def test_tracked_files_and_directories(self):
        text = "[source](../src/main.rs) [folder](../src/) [space](<A%20B.md>)"
        self.assertEqual([], LINKS.broken_links("docs/index.md", text, {"src/main.rs", "docs/A B.md"}))

    def test_missing_ignored_file_and_outside_tree(self):
        errors = LINKS.broken_links("docs/index.md", "[local](SESSION.md)\n[x](../../private.md)\n[x](/tmp/local.md)", {"docs/index.md"})
        self.assertEqual(3, len(errors))
        self.assertIn("docs/index.md:1:", errors[0])
        self.assertIn("docs/index.md:2:", errors[1])

    def test_external_fragments_and_code_examples(self):
        text = """[web](https://example.com/absent) [mail](mailto:x@example.com) [section](#heading)
`[code](missing.md)`
```md
[example](missing.md)
```
~~~
[example](missing.md)
~~~
[actual](real.md#heading)
"""
        self.assertEqual([], LINKS.broken_links("docs/index.md", text, {"docs/real.md"}))

    def test_reference_destinations_images_titles_and_parentheses(self):
        text = '[ref][label]\n[label]: ../README.md "title"\n![image](img(x).svg "title")\n[missing]: lost.md\n'
        errors = LINKS.broken_links("docs/index.md", text, {"README.md", "docs/img(x).svg"})
        self.assertEqual(1, len(errors))
        self.assertIn("docs/index.md:4: lost.md", errors[0])


if __name__ == "__main__":
    unittest.main()
