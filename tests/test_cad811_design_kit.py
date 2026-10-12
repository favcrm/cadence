"""Independent CAD-811 acceptance: shared light-theme drift is refused."""

from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class DesignKitAcceptance(unittest.TestCase):
    def test_shared_light_theme_disagreement_is_refused_without_output_writes(self):
        repo = Path(__file__).resolve().parents[1]
        sources = (
            "scripts/design-kit",
            "ui/src/styles.css",
            "ui/screen-ui/src/theme.css",
            "design/tokens.json",
            "design/tokens.css",
        )
        with tempfile.TemporaryDirectory(prefix="cad811-theme-refusal-") as directory:
            root = Path(directory)
            for relative in sources:
                destination = root / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(repo / relative, destination)

            outputs = (root / "design/tokens.json", root / "design/tokens.css")
            before = {path: path.read_bytes() for path in outputs}
            theme = root / "ui/screen-ui/src/theme.css"
            original = theme.read_text()
            # Anchor to the attribute block, not the identical media override.
            light_override = (
                ':root[data-theme="light"] {\n'
                '  color-scheme: light;\n'
                '  --color-ink-950: #f8f9fa;'
            )
            self.assertEqual(original.count(light_override), 1)
            theme.write_text(
                original.replace(
                    light_override,
                    light_override.replace("#f8f9fa", "#f8f9fb"),
                    1,
                )
            )

            result = subprocess.run(
                [sys.executable, str(root / "scripts/design-kit"), "--check"],
                cwd=root,
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
            diagnostic = result.stdout + result.stderr
            self.assertNotEqual(result.returncode, 0, diagnostic)
            for path, contents in before.items():
                self.assertEqual(path.read_bytes(), contents, str(path))
            # Missing @theme or stale tokens alone must not satisfy acceptance.
            self.assertIn("two light-theme blocks", diagnostic)
            self.assertIn("disagree", diagnostic)


if __name__ == "__main__":
    unittest.main()
