"""Packaging configuration checks for the Linux headless slice.

These tests validate the *configuration* of the AppImage and systemd packaging:
structure, permissions, naming, and the fact that nothing touches the Windows
release outputs. Set AIRFLASH_BUILD_APPIMAGE=1 to also run the real build.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import struct
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[1]
APPIMAGE = REPO / "packaging" / "linux" / "appimage"
SYSTEMD = REPO / "packaging" / "linux" / "systemd" / "user"
CONFIG = REPO / "packaging" / "linux" / "config"
LINUX_DOCS = REPO / "docs" / "LINUX.md"
BUILD_SCRIPT = APPIMAGE / "build-appimage.sh"
ARTIFACT_PATTERN = re.compile(r"^AirFlash-\d+\.\d+\.\d+(-[\w.]+)?-x86_64\.AppImage$")


def test_appdir_files_exist_with_executable_permissions():
    apprun = APPIMAGE / "AppRun"
    assert apprun.is_file(), "AppRun template is missing"
    assert os.access(apprun, os.X_OK), "AppRun must be committed executable"
    text = apprun.read_text(encoding="utf-8")
    assert text.startswith("#!"), "AppRun needs a shebang"
    assert "usr/bin/airflash-cli" in text, "AppRun must launch the CLI"
    assert "AIRFLASH_RUNTIME_DIR" in text, "AppRun must provide a runtime directory"
    # The wrapper must not assume a graphical session exists.
    assert "dbus" not in text.lower() or "DBUS" not in text
    assert (APPIMAGE / "airflash-cli.desktop").is_file(), "desktop entry template is missing"
    assert BUILD_SCRIPT.is_file(), "AppImage build script is missing"
    assert os.access(BUILD_SCRIPT, os.X_OK), "build script must be executable"


def test_desktop_entry_declares_the_minimum_metadata():
    entry = (APPIMAGE / "airflash-cli.desktop").read_text(encoding="utf-8")
    fields = dict(
        line.split("=", 1)
        for line in entry.splitlines()
        if "=" in line and not line.startswith("#")
    )
    assert fields["Type"] == "Application"
    assert fields["Name"] == "AirFlash"
    assert fields["Exec"] == "airflash-cli"
    assert fields["Icon"] == "airflash-cli"
    assert "Categories" in fields and "Audio" in fields["Categories"]
    assert fields["Terminal"] == "true", "the headless slice is a CLI; do not hide the terminal"
    # Version naming comes from the placeholder the build script substitutes.
    assert fields["X-AppImage-Version"] == "@VERSION@"
    assert "Version" in fields


def test_icon_is_a_real_png_matching_the_source_asset():
    source = (REPO / "desktop" / "AirFlash.App" / "Assets" / "app.png").read_bytes()
    header = source[:24]
    assert header[:8] == b"\x89PNG\r\n\x1a\n", "source asset is not a PNG"
    width, height = struct.unpack(">II", source[16:24])
    assert (width, height) == (512, 512), "AppImage icon must be 512x512"
    script = BUILD_SCRIPT.read_text(encoding="utf-8")
    assert "512x512" in script, "the build script must install the icon into the hicolor tree"
    assert ".DirIcon" in script, "AppDir needs .DirIcon for desktop integration"


def test_build_script_is_repeatable_and_never_touches_windows_outputs():
    script = BUILD_SCRIPT.read_text(encoding="utf-8")
    assert "set -euo pipefail" in script, "the build script must fail fast"
    for required in (
        "VERSION=",
        "TARGET=",
        "OUTPUT_DIR=",
        "features cli",
        "sha256sum",
        "rm -rf",
        "usrsuffix" if False else "usr/bin",
    ):
        assert required in script, f"build script is missing {required}"
    # Windows release artifacts are produced by scripts/build.ps1, not here.
    # Only executed lines count; comments may name them to say they are untouched.
    commands = "\n".join(
        line for line in script.splitlines() if line.strip() and not line.strip().startswith("#")
    )
    for forbidden in ("AirFlash.exe", "msi", "wixproj", "wix"):
        assert forbidden not in commands.lower(), f"Linux packaging must not produce {forbidden}"


def test_appimage_artifact_naming_is_derivable():
    script = BUILD_SCRIPT.read_text(encoding="utf-8")
    assert "AirFlash-" in script and "-$ARCH.AppImage" in script
    assert ARTIFACT_PATTERN.match("AirFlash-0.4.0-x86_64.AppImage")
    assert ARTIFACT_PATTERN.match("AirFlash-0.4.0-preview.1-x86_64.AppImage")
    assert not ARTIFACT_PATTERN.match("AirFlash-x86_64.AppImage"), "version must be in the name"
    assert not ARTIFACT_PATTERN.match("AirFlash-0.4.0.msi")


def test_systemd_user_units_are_rootless_and_configurable():
    for unit in (SYSTEMD / "airflash-cli.service", SYSTEMD / "airflash-cli@.service"):
        text = unit.read_text(encoding="utf-8")
        assert "[Service]" in text and "[Install]" in text
        assert "WantedBy=default.target" in text
        assert "Type=simple" in text
        assert "start --foreground" in text, "the service runs the CLI in the foreground"
        # A user unit needs no privilege escalation: no User= directive and no
        # privileged helper in the unit itself.
        assert "Restart=on-failure" in text and "RestartSec=" in text
        assert "KillSignal=SIGTERM" in text, "graceful teardown must be requested, not SIGKILL"
        assert "StandardOutput=journal" in text
        directives = [
            line.strip()
            for line in text.splitlines()
            if "=" in line and not line.strip().startswith(("#", ";"))
        ]
        assert not [d for d in directives if d.lower().startswith(("user=", "group="))]
        assert "sudo" not in text.lower()
        # Rootless: no User= directive (user units already run as the caller) and
        # nothing in the unit asks for elevated privileges.
        directives = [
            line.strip()
            for line in text.splitlines()
            if "=" in line and not line.strip().startswith(("#", ";"))
        ]
        assert not [d for d in directives if d.lower().startswith(("user=", "group="))]
        assert "sudo" not in text and "capsh" not in text
    unit = (SYSTEMD / "airflash-cli.service").read_text(encoding="utf-8")
    assert "EnvironmentFile=-" in unit, "optional env file must not fail the unit when missing"
    assert "%t/airflash" in unit, "state must live in the per-user runtime directory"
    assert "--config" in unit, "the service reads its session settings from a config file"
    template = (SYSTEMD / "airflash-cli@.service").read_text(encoding="utf-8")
    assert "%i" in template, "the template instance must select a profile"


def test_example_configs_are_valid_and_cover_the_audio_choice():
    settings = json.loads((CONFIG / "cli.json.example").read_text(encoding="utf-8"))
    assert settings["host"], "an example receiver is required"
    assert settings["source"] in {"simulated", "file", "loopback"}
    assert settings["timing"] in {"ptp", "ntp", "auto"}
    # Privileged PTP ports are not available to unprivileged user services;
    # "auto" is allowed because it falls back to NTP there.
    assert settings["timing"] in {"ntp", "auto"}, "the example config must work without privileges"
    assert 0.0 <= settings["gain"] <= 1.0
    env = (CONFIG / "cli.env.example").read_text(encoding="utf-8")
    assert "DBUS_SESSION_BUS_ADDRESS" in env, "the audio session environment must be documented"


def test_linux_docs_cover_the_documented_boundaries():
    assert LINUX_DOCS.is_file(), "docs/LINUX.md is required"
    text = LINUX_DOCS.read_text(encoding="utf-8").lower()
    for section in (
        "systemd --user",
        "appimage",
        "pipewire",
        "limitations",
        "discover",
        "airflash-cli start",
        "airflash-cli stop",
        "audio",
    ):
        assert section in text, f"docs/LINUX.md must document {section}"


def test_appimage_script_supports_a_gui_variant():
    """The full variant adds the GUI; the backend variant must stay CLI-only."""
    script = BUILD_SCRIPT.read_text(encoding="utf-8")
    for required in (
        "VARIANT=${VARIANT:-backend}",
        "full) SUFFIX=\"-full\" ;;",
        "dotnet publish",
        "usr/lib/airflash-ui",
        "usr/bin/airflash-ui",
    ):
        assert required in script, f"build script is missing {required}"
    apprun = (APPIMAGE / "AppRun").read_text(encoding="utf-8")
    assert "airflash-ui" in apprun, "AppRun must be able to launch the GUI"
    assert "is_cli_command" in apprun, "AppRun must dispatch CLI commands"
    # A bad variant fails loudly instead of silently building the backend image.
    assert "VARIANT must be backend or full" in script


@pytest.mark.skipif(
    os.environ.get("AIRFLASH_BUILD_APPIMAGE") != "1",
    reason="set AIRFLASH_BUILD_APPIMAGE=1 to run the AppImage build",
)
def test_full_appimage_build_includes_the_gui(tmp_path):
    """VARIANT=full adds the panel to the same artifact family."""
    if shutil.which("cargo") is None or shutil.which("dotnet") is None:
        pytest.skip("cargo and dotnet are required")
    build = tmp_path / "build"
    out = tmp_path / "out"
    result = subprocess.run(
        ["bash", str(BUILD_SCRIPT)],
        env={
            **os.environ,
            "VARIANT": "full",
            "VERSION": "0.0.0.test",
        "BUILD_DIR": str(build),
        "OUTPUT_DIR": str(out),
    },
        capture_output=True,
        text=True,
        timeout=3600,
    )
    assert result.returncode == 0, f"full build failed:\n{result.stdout}\n{result.stderr}"
    artifacts = sorted(out.glob("*.AppImage"))
    assert len(artifacts) == 1, f"expected one AppImage, got {artifacts}"
    assert ARTIFACT_PATTERN.match(artifacts[0].name.replace("-full", "")), artifacts[0].name
    assert artifacts[0].name.endswith("-full.AppImage"), "the full image must be distinguishable"
    appdir = build / "AirFlash.AppDir"
    for relative in (
        "AppRun",
        "VERSION",
        ".DirIcon",
        "airflash-cli.desktop",
        "airflash-ui.desktop",
        "airflash-cli.png",
        "usr/bin/airflash-cli",
        "usr/bin/airflash-engine",
        "usr/bin/airflash-ui",
        "usr/lib/airflash-ui/AirFlash.UI",
        "usr/share/applications/airflash-ui.desktop",
        "usr/share/icons/hicolor/512x512/apps/airflash-cli.png",
    ):
        path = appdir / relative
        assert path.is_file(), f"full AppDir is missing {relative}"
    assert os.access(appdir / "usr/bin/airflash-ui", os.X_OK)
    # The two desktop entries must launch different binaries.
    gui_entry = (appdir / "airflash-ui.desktop").read_text(encoding="utf-8")
    assert "Exec=airflash-ui" in gui_entry
    assert "Terminal=false" in gui_entry


@pytest.mark.skipif(
    os.environ.get("AIRFLASH_BUILD_APPIMAGE") != "1",
    reason="set AIRFLASH_BUILD_APPIMAGE=1 to run the AppImage build",
)
def test_appimage_build_produces_a_named_artifact(tmp_path):
    if shutil.which("cargo") is None or shutil.which("curl") is None:
        pytest.skip("cargo and curl are required")
    build = tmp_path / "build"
    out = tmp_path / "out"
    result = subprocess.run(
        ["bash", str(BUILD_SCRIPT)],
        env={
            **os.environ,
            "VERSION": "0.0.0.test",
        "BUILD_DIR": str(build),
        "OUTPUT_DIR": str(out),
    },
        capture_output=True,
        text=True,
        timeout=1800,
    )
    assert result.returncode == 0, f"build script failed:\n{result.stdout}\n{result.stderr}"
    artifacts = sorted(out.glob("*.AppImage"))
    assert len(artifacts) == 1, f"expected one AppImage, got {artifacts}"
    assert ARTIFACT_PATTERN.match(artifacts[0].name), artifacts[0].name
    assert (out / "SHA256SUMS.txt").is_file()
    # The AppDir structure is inspectable without executing the artifact.
    appdir = build / "AirFlash.AppDir"
    for relative in (
        "AppRun",
        "VERSION",
        ".DirIcon",
        "airflash-cli.desktop",
        "airflash-cli.png",
        "usr/bin/airflash-cli",
        "usr/bin/airflash-engine",
        "usr/share/applications/airflash-cli.desktop",
        "usr/share/icons/hicolor/512x512/apps/airflash-cli.png",
        "usr/share/doc/airflash-cli/README",
        "usr/share/licenses/airflash-cli/LICENSE-GPLv3",
    ):
        path = appdir / relative
        assert path.is_file(), f"AppDir is missing {relative}"
    assert os.access(appdir / "AppRun", os.X_OK)
    assert (appdir / "VERSION").read_text(encoding="utf-8").strip() == "0.0.0.test"
    # The desktop entry is version-substituted, not a leftover placeholder.
    entry = (appdir / "airflash-cli.desktop").read_text(encoding="utf-8")
    assert "@VERSION@" not in entry
    assert "X-AppImage-Version=0.0.0.test" in entry
    checksums = (out / "SHA256SUMS.txt").read_text(encoding="utf-8")
    assert re.fullmatch(r"[0-9a-f]{64}  " + re.escape(artifacts[0].name) + r"\n", checksums)
