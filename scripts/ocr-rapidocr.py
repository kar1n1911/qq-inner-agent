#!/usr/bin/env python3
"""Single-image RapidOCR bridge: stdout is one JSON object, scores are 0..1."""

import json
import math
import os
import sys


def main():
    # Keep stdout redirected for the process lifetime, including native ONNX
    # logging and shutdown hooks. Only this duplicated descriptor carries JSON.
    sys.stdout.flush()
    with os.fdopen(os.dup(sys.stdout.fileno()), "w", encoding="utf-8") as output:
        os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
        try:
            if len(sys.argv) != 2:
                raise ValueError("usage: ocr-rapidocr.py IMAGE")
            from rapidocr_onnxruntime import RapidOCR

            # Reloading models per invocation costs ~2–3s; a future persistent
            # process can retain the engine while preserving this JSON contract.
            result, _ = RapidOCR()(sys.argv[1])
            lines = []
            for _, text, score in result or []:
                score = float(score)
                if not math.isfinite(score) or not 0.0 <= score <= 1.0:
                    raise ValueError("invalid line confidence")
                lines.append({"text": str(text), "score": score})
            payload = {
                "lines": lines,
                "text": "\n".join(line["text"] for line in lines),
                "confidence": sum(line["score"] for line in lines) / len(lines)
                if lines else 0.0,
            }
            print(json.dumps(payload, ensure_ascii=False, allow_nan=False), file=output)
            return 0
        except Exception as exc:
            print(json.dumps({"error": str(exc)}, ensure_ascii=False), file=sys.stderr)
            return 1


if __name__ == "__main__":
    sys.exit(main())
