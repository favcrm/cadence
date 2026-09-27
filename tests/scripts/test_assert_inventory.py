"""Lexical comment regressions for the assertion audit's actual scanner and CLI."""
import importlib.machinery
import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "assert-inventory"
LOADER = importlib.machinery.SourceFileLoader("assert_inventory", str(SCRIPT))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
SCANNER = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(SCANNER)


class AssertionInventoryTests(unittest.TestCase):
    def rows(self, source):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "case.rs"
            path.write_text(source, encoding="utf-8")
            prefix = f"{path}::"
            return [row.removeprefix(prefix) for row in SCANNER.inventory(path)]

    def test_nested_depths_hide_comment_assertions(self):
        for depth in (2, 3, 5):
            with self.subTest(depth=depth):
                comment = "/*" * depth + "*/" * (depth - 1)
                source = (
                    "#[test]\nfn real() { " + comment
                    + ' assert!(false, "comment only"); */ '
                    + 'assert_eq!(value, 7, "real message"); }'
                )
                self.assertEqual(self.rows(source), [
                    'real (test) | assert_eq! | value, 7, "real message"',
                ])

    def test_nested_comment_hides_test_function(self):
        source = (
            "/* outer /* inner */\n#[test]\nfn hidden() { panic!(\"hidden\"); }\n*/\n"
            "#[test]\nfn real() { assert_ne!(left, right); }\n"
        )
        self.assertEqual(self.rows(source), ["real (test) | assert_ne! | left, right"])

    def test_unterminated_outer_comment_masks_to_eof(self):
        for suffix in ("", "雪\n", "/* deeper */"):
            with self.subTest(suffix=suffix):
                source = "/* outer /* inner */\nfn hidden() { assert!(false); }" + suffix
                self.assertEqual(self.rows(source), [])
                masked = SCANNER.mask_src(source)
                self.assertEqual(masked, "".join("\n" if c == "\n" else " " for c in source))

    def test_mask_preserves_unicode_offsets_newlines_and_outside_source(self):
        before, after = "fn before() {}\n", "\nfn after() {}"
        comment = "/* 雪\n/* λ */\n quoted 'x' and \"text\" */"
        source = before + comment + after
        masked = SCANNER.mask_src(source)
        expected = before + "".join("\n" if c == "\n" else " " for c in comment) + after
        self.assertEqual(masked, expected)
        self.assertEqual(len(masked), len(source))
        self.assertEqual([i for i, c in enumerate(masked) if c == "\n"],
                         [i for i, c in enumerate(source) if c == "\n"])

    def test_comment_newline_keeps_following_function_boundary(self):
        source = "fn first() { assert!(one); }/*\n*/fn second() { assert!(two); }"
        self.assertEqual(self.rows(source), [
            "first | assert! | one", "second | assert! | two",
        ])

    def test_adjacent_comments_and_quotes_do_not_change_comment_depth(self):
        comment = '/* " /* nested */ assert!(false); " */'
        source = "fn real() { " + comment + "/*'/* nested */ panic!(false);'*/assert!(ok); }"
        self.assertEqual(self.rows(source), ["real | assert! | ok"])

    def test_closed_comment_at_eof_and_empty_comment(self):
        for comment in ("/**/", "/* /* nested */ */", "/*\n雪\n*/"):
            with self.subTest(comment=comment):
                self.assertEqual(SCANNER.mask_src("x" + comment),
                                 "x" + "".join("\n" if c == "\n" else " " for c in comment))
        self.assertEqual(SCANNER.mask_src(""), "")

    def test_literal_delimiters_and_lifetimes_keep_existing_behavior(self):
        source = r'''fn real<'a>(value: &'a str) {
    let ordinary = "/* assert!(false); */ escaped \" //";
    let raw0 = r"/* panic!(false); */";
    let raw1 = r#"# /* assert!(false); */"#;
    let raw3 = r###"/* */ "## assert!(false);"###;
    let byte0 = br"/* */";
    let byte1 = br#"# /* */"#;
    let byte3 = br###"/* */ "## panic!(false);"###;
    let brace = '}'; let quote = '\''; let slash = '/';
    assert_matches!(value, Some("/* literal */"), "message preserved");
}'''
        self.assertEqual(self.rows(source), [
            'real | assert_matches! | value, Some("/* literal */"), "message preserved"',
        ])
        masked = SCANNER.mask_src(source)
        self.assertIn("fn real<'a>(value: &'a str)", masked)
        self.assertEqual(len(masked), len(source))

    def test_cli_emits_only_real_assertion_with_relative_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "cases").mkdir()
            (root / "cases" / "nested.rs").write_text(
                "/* outer /* inner */\n#[test]\nfn hidden() { assert!(false); }\n*/\n"
                '#[test]\nfn real() { assert_eq!(1, 1, "kept"); }\n', encoding="utf-8",
            )
            result = subprocess.run([sys.executable, str(SCRIPT), "cases"], cwd=root,
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stderr, "")
            self.assertEqual(result.stdout,
                             'cases/nested.rs::real (test) | assert_eq! | 1, 1, "kept"\n')


if __name__ == "__main__":
    unittest.main()
