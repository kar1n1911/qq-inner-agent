"""Bridge contract tests without installing ONNX or downloading models."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("ocr-rapidocr.py")


class BridgeTests(unittest.TestCase):
    def run_bridge(self, result="[]", failure=False, args=None):
        with tempfile.TemporaryDirectory() as directory:
            Path(directory, "rapidocr_onnxruntime.py").write_text(
                "import atexit, os\n"
                "print('import log')\n"
                "os.write(1, b'native log\\n')\n"
                "atexit.register(lambda: print('shutdown log'))\n"
                "class RapidOCR:\n"
                "    def __init__(self):\n"
                "        print('model loading')\n"
                "    def __call__(self, image):\n"
                "        assert image == 'image with spaces.jpg'\n"
                + ("        raise RuntimeError('inference failed')\n" if failure
                   else f"        return {result}, [0.1]\n"),
                encoding="utf-8",
            )
            return subprocess.run(
                [sys.executable, str(SCRIPT), *(args if args is not None
                                               else ["image with spaces.jpg"])],
                env={**os.environ, "PYTHONPATH": directory},
                capture_output=True, text=True, encoding="utf-8", check=False,
                timeout=10,
            )

    def test_lines_mean_and_log_isolation(self):
        run = self.run_bridge("[[[], '肺鼠疫', 0.9], [[], '上线', 0.8]]")
        self.assertEqual(run.returncode, 0, run.stderr)
        payload = json.loads(run.stdout)
        self.assertEqual(payload["lines"], [
            {"text": "肺鼠疫", "score": 0.9}, {"text": "上线", "score": 0.8},
        ])
        self.assertEqual(payload["text"], "肺鼠疫\n上线")
        self.assertAlmostEqual(payload["confidence"], 0.85)
        for log in ["import log", "native log", "model loading", "shutdown log"]:
            self.assertIn(log, run.stderr)

    def test_empty(self):
        for result in ["None", "[]"]:
            run = self.run_bridge(result)
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(json.loads(run.stdout), {
                "lines": [], "text": "", "confidence": 0.0,
            })

    def test_errors(self):
        for kwargs in [
            {"failure": True}, {"args": []}, {"args": ["a", "b"]},
            {"result": "[[[], 'bad', float('nan')]]"},
            {"result": "[[[], 'bad', 1.1]]"},
        ]:
            run = self.run_bridge(**kwargs)
            self.assertNotEqual(run.returncode, 0)
            self.assertEqual(run.stdout, "")
            errors = [json.loads(line) for line in run.stderr.splitlines()
                      if line.startswith('{"error":')]
            self.assertEqual(len(errors), 1, run.stderr)
            self.assertTrue(errors[0]["error"])


if __name__ == "__main__":
    unittest.main()
