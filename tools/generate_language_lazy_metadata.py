#!/usr/bin/env python3
import json
import pathlib
import re
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parent.parent
DATA = ROOT / "data"


def git_show(relpath: str) -> str:
    result = subprocess.run(
        ["git", "show", f"HEAD:{relpath}"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout


def extract_match_list(source: str, field: str) -> list[str]:
    match = re.search(rf"{field}\s*=\s*\{{", source)
    if not match:
        return []
    start = match.end() - 1
    depth = 0
    end = start
    for idx in range(start, len(source)):
        ch = source[idx]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                end = idx
                break
    block = source[start : end + 1]
    return [text for _, text in re.findall(r"(['\"])(.*?)\1", block, re.S)]


def main() -> int:
    for path in sorted((DATA / "plugins").glob("language_*.lua")):
        rel = path.relative_to(ROOT).as_posix()
        source = git_show(rel)
        metadata = {
            "files": extract_match_list(source, "files"),
            "headers": extract_match_list(source, "headers"),
        }
        out_path = path.with_suffix(".lazy.json")
        out_path.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
