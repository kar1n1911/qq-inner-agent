#!/usr/bin/env python3
"""Turn an LCCC dialogue split into a deterministic replay sample.

LCCC (MIT, thu-coai) is a corpus of Chinese short conversations: each line is
a JSON array of space-separated utterances. It has no speaker ids and no
timestamps, so this script invents both. Those inventions are simulator
assumptions, not data facts: they must be stated wherever results are read.

Usage:
  python3 scripts/lab/lccc_replay_prep.py \
      --input ~/lab/group-onboarding/corpus/lccc_base_test.jsonl.gz \
      --output ~/lab/group-onboarding/corpus/replay_sample.jsonl \
      --limit 200 --speakers 2 --gap-seconds 25
"""

from __future__ import annotations

import argparse
import gzip
import json
import sys
from pathlib import Path

ASCII_WORD = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789")
BASE_TS = 1_700_000_000.0


def detokenize(tokens: str) -> str:
    """Join LCCC word tokens into readable text.

    LCCC is pre-tokenized with spaces. Joining without a space is right for
    Chinese, but it would merge Latin words, so a space is kept only when both
    neighbouring characters are ASCII word characters.
    """
    tokens = tokens.split()
    if not tokens:
        return ""
    out = [tokens[0]]
    for token in tokens[1:]:
        left, right = out[-1][-1:], token[:1]
        out.append(" " if left in ASCII_WORD and right in ASCII_WORD else "")
        out.append(token)
    return "".join(out)


def load(path: Path):
    opener = gzip.open if path.suffix == ".gz" else open
    with opener(path, "rt", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                continue


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--limit", type=int, default=200)
    parser.add_argument("--speakers", type=int, default=2)
    parser.add_argument("--gap-seconds", type=int, default=25)
    parser.add_argument("--max-chars", type=int, default=200)
    parser.add_argument("--seed", type=int, default=7)
    args = parser.parse_args()

    names = [f"member-{chr(ord('a') + i)}" for i in range(max(1, args.speakers))]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    written = skipped = 0
    with args.output.open("w", encoding="utf-8") as out:
        for index, dialog in enumerate(load(args.input)):
            if written >= args.limit:
                break
            if not isinstance(dialog, list):
                skipped += 1
                continue
            turns = []
            ts = BASE_TS + index * 3600.0
            for turn_index, raw in enumerate(dialog):
                text = detokenize(raw) if isinstance(raw, str) else ""
                text = text.strip()
                if not text or len(text) > args.max_chars:
                    continue
                turns.append(
                    {
                        "speaker": names[turn_index % len(names)],
                        "text": text,
                        "ts": ts,
                    }
                )
                ts += args.gap_seconds
            if len(turns) < 2:
                skipped += 1
                continue
            out.write(
                json.dumps(
                    {
                        "dialog_id": f"lccc-base-test-{index:06d}",
                        "source": "lccc-base-test",
                        "assumptions": {
                            "speakers": "round-robin over member-*",
                            "timing": f"fixed {args.gap_seconds}s between turns",
                        },
                        "turns": turns,
                    },
                    ensure_ascii=False,
                )
                + "\n"
            )
            written += 1

    print(
        f"wrote {written} dialogues to {args.output} (skipped {skipped})",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
