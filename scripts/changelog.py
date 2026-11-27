"""Print one release's section of CHANGELOG.md (without its heading), for the release notes.

usage: changelog.py <tag>        e.g. changelog.py v0.2.0

Prints nothing (and exits 0) if there's no section for the tag, so a release still gets
GitHub's generated notes.
"""

import pathlib
import sys


def main() -> None:
    tag = sys.argv[1]
    lines = pathlib.Path(__file__).resolve().parent.parent.joinpath("CHANGELOG.md").read_text(encoding="utf-8").splitlines()
    out, inside = [], False
    for line in lines:
        if line.startswith("## "):
            if inside:
                break
            inside = line[3:].strip() == tag
            continue
        if inside:
            out.append(line)
    print("\n".join(out).strip())


if __name__ == "__main__":
    main()
