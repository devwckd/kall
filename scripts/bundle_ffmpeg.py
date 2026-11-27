"""Make the Windows installer carry FFmpeg's DLLs: list the ones build.rs copied next to
kall.exe as cargo-packager resources, installed next to kall.exe.

usage: bundle_ffmpeg.py <crates/client/Cargo.toml>

Only for builds with the `ffmpeg` feature (the release workflow runs it on Windows).
"""

import pathlib
import sys

DLLS = ("avcodec", "avformat", "avutil", "swscale", "swresample")


def main() -> None:
    manifest = pathlib.Path(sys.argv[1])
    release = manifest.resolve().parent.parent.parent / "target" / "release"
    found = sorted(p.name for p in release.glob("*.dll") if p.name.split("-")[0] in DLLS)
    if len(found) != len(DLLS):
        sys.exit(f"expected {len(DLLS)} FFmpeg DLLs in {release}, found {found}")
    entries = ", ".join(f'{{ src = "../../target/release/{name}", target = "." }}' for name in found)
    text = manifest.read_text(encoding="utf-8")
    anchor = 'binaries = [{ path = "kall", main = true }]'
    if anchor not in text:
        sys.exit("couldn't find the packager binaries line")
    text = text.replace(anchor, f"{anchor}\nresources = [{entries}]", 1)
    manifest.write_text(text, encoding="utf-8")
    print("bundling", ", ".join(found))


if __name__ == "__main__":
    main()
