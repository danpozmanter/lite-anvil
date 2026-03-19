#!/usr/bin/env python3
import json
import pathlib
import re
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parent.parent
DATA = ROOT / "data"

KEEP_COMMENT_PATTERNS = [
    re.compile(r"^--\s*mod-version:.*$"),
    re.compile(r"^--\s*priority:.*$"),
]


def keep_comment(line: str) -> bool:
    return any(pattern.match(line) for pattern in KEEP_COMMENT_PATTERNS)


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


def write_language_lazy_metadata(path: pathlib.Path, source: str) -> None:
    if not path.name.startswith("language_"):
        return
    metadata = {
        "files": extract_match_list(source, "files"),
        "headers": extract_match_list(source, "headers"),
    }
    path.with_suffix(".lazy.json").write_text(
        json.dumps(metadata, indent=2) + "\n", encoding="utf-8"
    )


def compile_and_wrap(path: pathlib.Path) -> None:
    rel = path.relative_to(DATA).as_posix()
    luac_path = path.with_suffix(".luac")
    source = path.read_text(encoding="utf-8")
    write_language_lazy_metadata(path, source)
    subprocess.run(["luac", "-o", str(luac_path), str(path)], check=True)

    kept = []
    for line in source.splitlines():
        if keep_comment(line):
            kept.append(line)

    stub_lines = kept + [f'return assert(loadfile(DATADIR .. "/{rel[:-4]}.luac"))()']
    path.write_text("\n".join(stub_lines) + "\n", encoding="utf-8")


def main() -> int:
    if not DATA.is_dir():
        print(f"missing data dir: {DATA}", file=sys.stderr)
        return 1

    for path in sorted(DATA.rglob("*.lua")):
        compile_and_wrap(path)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
