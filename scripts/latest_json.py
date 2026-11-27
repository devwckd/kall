"""Write kall's latest.json (the auto-updater manifest) from the release build artifacts.

usage: latest_json.py <artifacts dir> <tag> <download base url>

Each artifacts/dist-<platform>/ folder holds one platform's packages and their .sig files
(cargo-packager writes those when signing). The updater installs:
  windows: the NSIS setup .exe      (format "nsis")
  linux:   the .AppImage            (format "appimage")
  macos:   the .app.tar.gz bundle   (format "app")
"""

import json
import pathlib
import sys
from datetime import datetime, timezone

UPDATABLE = [("-setup.exe", "nsis"), (".AppImage", "appimage"), (".app.tar.gz", "app")]


def main() -> None:
    root, tag, base = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3].rstrip("/")
    platforms = {}
    for folder in sorted(root.glob("dist-*")):
        platform = folder.name.removeprefix("dist-")
        for path in sorted(folder.iterdir()):
            fmt = next((f for suffix, f in UPDATABLE if path.name.endswith(suffix)), None)
            sig = path.with_name(path.name + ".sig")
            if fmt and sig.exists():
                platforms[platform] = {"signature": sig.read_text().strip(), "url": f"{base}/{path.name}", "format": fmt}
    if not platforms:
        sys.exit("no signed update bundles found (is CARGO_PACKAGER_SIGN_PRIVATE_KEY set?)")
    manifest = {
        "version": tag.lstrip("v"),
        "notes": f"kall {tag}",
        "pub_date": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": platforms,
    }
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
