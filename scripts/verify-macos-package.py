"""Verify the real DMG and its universal macOS payload; no install claim."""

import hashlib
import json
import plistlib
import shutil
import subprocess
import tempfile
from pathlib import Path


def command(*args):
    result = subprocess.run(args, check=True, capture_output=True, text=True)
    return (result.stdout + result.stderr).strip()


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verify_app(app, expected):
    with (app / "Contents/Info.plist").open("rb") as stream:
        info = plistlib.load(stream)
    assert info["CFBundleShortVersionString"] == expected["version"], info
    assert info["CFBundleIdentifier"] == expected["identifier"], info
    binary = app / "Contents/MacOS" / info["CFBundleExecutable"]
    architectures = command("lipo", "-archs", str(binary)).split()
    assert set(architectures) == {"x86_64", "arm64"}, architectures
    binary_format = command("file", str(binary))
    assert "Mach-O" in binary_format and "universal" in binary_format, binary_format
    command("codesign", "--verify", "--deep", "--strict", str(app))
    signature = command("codesign", "-d", "--verbose=4", str(app))
    assert "Signature=adhoc" in signature, signature
    return {
        "version": info["CFBundleShortVersionString"],
        "identifier": info["CFBundleIdentifier"],
        "minimum_system_version": info.get("LSMinimumSystemVersion"),
        "architectures": sorted(architectures),
        "binary_format": binary_format,
        "binary_sha256": digest(binary),
        "signature": signature,
    }


def main():
    root = Path(__file__).resolve().parent.parent
    expected = json.loads((root / "src-tauri/tauri.conf.json").read_text())
    bundle = root / "src-tauri/target/universal-apple-darwin/release/bundle"
    images = list((bundle / "dmg").glob("*.dmg"))
    assert len(images) == 1, images
    image = images[0]
    with image.open("rb") as stream:
        stream.seek(-512, 2)
        assert stream.read(4) == b"koly", "DMG UDIF trailer missing"
    image_verification = command("hdiutil", "verify", str(image))
    built_app = verify_app(bundle / "macos" / "LLM Gateway.app", expected)
    with tempfile.TemporaryDirectory(prefix="llmgateway-dmg-") as mount:
        command("hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", mount, str(image))
        try:
            mounted_app = verify_app(Path(mount) / "LLM Gateway.app", expected)
            assert mounted_app["binary_sha256"] == built_app["binary_sha256"]
        finally:
            command("hdiutil", "detach", mount)
    output = root / "macos-artifacts"
    output.mkdir(exist_ok=False)
    delivered = output / f"LLM-Gateway_{expected['version']}_universal.dmg"
    shutil.copy2(image, delivered)
    sha256 = digest(delivered)
    evidence = {
        "source_base_commit": "9fe371308435c4a78d23d8374b9e2ffb9197827b",
        "build_commit": command("git", "rev-parse", "HEAD"),
        "installer": delivered.name,
        "bytes": delivered.stat().st_size,
        "sha256": sha256,
        "format": "Apple UDIF disk image (koly trailer and hdiutil verified)",
        "hdiutil_verification": image_verification,
        "app": mounted_app,
        "notarized": False,
        "real_mac_installation_tested": False,
        "known_limitations": [
            "Ad-hoc signature has no Apple Developer identity or notarization; Gatekeeper approval may be needed.",
            "Built-in Mihomo installation and login autostart are implemented for Windows only.",
        ],
    }
    (output / "verification.json").write_text(json.dumps(evidence, indent=2) + "\n")
    (output / "SHA256SUMS.txt").write_text(f"{sha256}  {delivered.name}\n")
    print(json.dumps(evidence, indent=2))


if __name__ == "__main__":
    main()
