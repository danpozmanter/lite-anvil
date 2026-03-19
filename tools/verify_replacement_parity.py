#!/usr/bin/env python3
from __future__ import annotations

import glob
import pathlib
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parent.parent


def run(cmd: list[str], *, cwd: pathlib.Path | None = None) -> str:
    result = subprocess.run(
        cmd,
        cwd=str(cwd or ROOT),
        text=True,
        capture_output=True,
    )
    if result.returncode != 0:
        raise SystemExit(
            f"command failed: {' '.join(cmd)}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result.stdout


def assert_language_wrappers_asset_backed() -> None:
    wrappers = sorted(glob.glob(str(ROOT / "data" / "plugins" / "language_*.lua")))
    luac_backed: list[str] = []
    asset_backed = 0
    for path in wrappers:
      text = pathlib.Path(path).read_text()
      if "add_from_asset(" in text:
          asset_backed += 1
      if ".luac" in text:
          luac_backed.append(path)
    if luac_backed:
        raise SystemExit("language wrappers still load .luac:\n" + "\n".join(luac_backed))
    print(f"language wrappers asset-backed: {asset_backed}/{len(wrappers)}")


def assert_language_inventory_matches_v0147() -> None:
    old = run(
        ["git", "ls-tree", "-r", "--name-only", "v0.14.7", "--", "data/plugins"],
        cwd=ROOT,
    ).splitlines()
    old_langs = sorted(
        pathlib.Path(p).stem for p in old if pathlib.Path(p).name.startswith("language_")
    )
    new_langs = sorted(
        pathlib.Path(p).stem for p in glob.glob(str(ROOT / "data" / "plugins" / "language_*.lua"))
    )
    if old_langs != new_langs:
        raise SystemExit("language inventory drifted from v0.14.7")
    print(f"language inventory matches v0.14.7: {len(new_langs)}")


def assert_startup_smoke_clean() -> None:
    out = pathlib.Path("/tmp/lite-anvil-parity.out")
    err = pathlib.Path("/tmp/lite-anvil-parity.err")
    cmd = (
        "env SDL_VIDEODRIVER=dummy timeout 5s cargo run --quiet "
        f">{out} 2>{err}"
    )
    subprocess.run(["bash", "-lc", cmd], cwd=str(ROOT), check=False)
    if err.exists() and err.read_text():
        raise SystemExit(f"startup smoke emitted stderr:\n{err.read_text()}")
    print("startup smoke stderr: clean")


def main() -> None:
    assert_language_inventory_matches_v0147()
    assert_language_wrappers_asset_backed()
    assert_startup_smoke_clean()


if __name__ == "__main__":
    main()
