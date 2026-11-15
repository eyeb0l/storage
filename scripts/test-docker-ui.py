#!/usr/bin/env python3
"""Exercise real Tauri IPC/UI using only a fake home and fake Docker. Build storage-app first.

Requires an active macOS GUI session. --keep-open preserves the fixture and window for visual QA;
the printed PID belongs only to this test instance. Default runs terminate their own instance.
"""
import argparse
import json
import os
import plistlib
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scenario", choices=["stable", "changed", "failure", "empty", "cancel"], default="stable")
    parser.add_argument("--keep-open", action="store_true")
    parser.add_argument("--width", type=int, default=920)
    parser.add_argument("--height", type=int, default=900)
    parser.add_argument("--visual-state", choices=["inventory", "details", "inventory-bottom", "review", "review-bottom"], default="inventory")
    parser.add_argument("--snapshot", type=Path, help="Save a PNG captured from the running native WebKit view")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.width < 720 or args.height < 560:
        parser.error("The minimum supported window is 720x560")
    if args.visual_state != "inventory" and args.scenario != "stable":
        parser.error("The review visual state requires the stable scenario")
    home = Path(tempfile.mkdtemp(prefix="storage-docker-ui-")).resolve()
    container = "a" * 64
    image = "sha256:" + "b" * 64

    def write(name, value):
        (home / name).write_text(json.dumps(value))

    write("context.json", [{"Name":"desktop-linux", "Endpoints":{"docker":{"Host":f"unix://{home}/.docker/run/docker.sock"}}}])
    write("engine.json", "mock-desktop-engine")
    (home / "containers").write_text(container if args.scenario != "empty" else "")
    (home / "images").write_text(f"{image}\n{image}" if args.scenario != "empty" else "")
    write(f"{container}.json", [{"Id":container, "Name":"/example-service", "Image":image,
          "Created":"2026-11-15T09:00:00Z", "SizeRw":None,
          "State":{"Status":"exited","Running":False},
          "Config":{"Env":["SECRET=never-expose-this-secret"], "Labels":{"com.docker.compose.project":'<img src=x onerror="window.injected=true">'}},
          "Mounts":[{"Type":"volume","Name":"keep-data","Destination":"/data","Source":"/private/never-expose-this-secret"}]}])
    original = [{"Id":image,"Created":"2026-11-13T12:00:00Z","Size":3_000_000,"RepoTags":["example:latest"],"RepoDigests":[]}]
    write(f"{image}.json", original)
    if args.scenario == "changed":
        write("changed-image.json", [{**original[0], "RepoTags":["new:tag"]}])
        (home / "change-after-inspect").touch()
    if args.scenario == "failure":
        (home / "fail-info").touch()
    if args.scenario == "cancel":
        (home / "block-info").touch()
    disk = home / "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"
    disk.parent.mkdir(parents=True)
    disk.write_bytes(b"x" * 1_200_000)
    executable = home / "docker"
    shutil.copyfile(ROOT / "crates/storage-core/tests/fixtures/docker-cli.sh", executable)
    executable.chmod(0o700)
    script = home / "ui-test.js"
    script.write_text(f"const SCENARIO={json.dumps(args.scenario)}; const VISUAL_STATE={json.dumps(args.visual_state)}; const CAPTURE={json.dumps(bool(args.snapshot))}; const EXPECTED_SIZE={json.dumps([args.width,args.height])};\n" + (ROOT / "scripts/docker-ui-test.js").read_text())
    report = home / "report.json"
    env = {**os.environ, "HOME":str(home), "STORAGE_UI_SCRIPT":str(script),
           "STORAGE_UI_REPORT":str(report), "STORAGE_DOCKER_EXECUTABLE":str(executable),
           "STORAGE_UI_SIZE":f"{args.width}x{args.height}",
           "STORAGE_UI_SNAPSHOT":str(home / "native.tiff"),
           "DOCKER_HOST":"tcp://unreviewed.invalid:2375", "DOCKER_CONTEXT":"unreviewed",
           "DOCKER_CONFIG":"/tmp/unreviewed-config", "BUILDX_BUILDER":"unreviewed", "BUILDKIT_HOST":"tcp://unreviewed.invalid:1234"}
    log = (home / "native.log").open("w")
    # A named temporary bundle makes this particular test window available to native AX/visual
    # tools without launching or altering an installed Storage app.
    app = home / "StorageDockerUITest.app"
    binary = app / "Contents/MacOS/StorageDockerUITest"
    binary.parent.mkdir(parents=True)
    shutil.copy2(ROOT / "target/debug/storage", binary)
    (app / "Contents/Info.plist").write_bytes(plistlib.dumps({
        "CFBundleExecutable":"StorageDockerUITest", "CFBundleIdentifier":"dev.storage.docker-ui-test",
        "CFBundleName":"Storage Docker UI Test", "CFBundlePackageType":"APPL",
        "NSHighResolutionCapable":True,
    }))
    process = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=log)
    passed = False
    try:
        deadline = time.monotonic() + 60
        while not report.exists():
            if process.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError(f"Native test stopped or timed out; see {home}/native.log")
            time.sleep(0.05)  # Wait for a concrete report, never a fixed settling delay.
        result = json.loads(report.read_text())
        if args.snapshot and result["status"] == "passed":
            args.snapshot.parent.mkdir(parents=True, exist_ok=True)
            subprocess.run(["/usr/bin/sips", "-s", "format", "png", str(home / "native.tiff"), "--out", str(args.snapshot.resolve())], check=True, capture_output=True)
            result["snapshot"] = str(args.snapshot.resolve())
        calls = (home / "calls").read_text().splitlines() if (home / "calls").exists() else []
        assert all(call.startswith(("context inspect ", "info --format ", "container ls ", "image ls ", "container inspect ", "image inspect ")) for call in calls), calls
        assert disk.exists() and disk.stat().st_size == 1_200_000
        result.update(pid=process.pid, fixture=str(home), app=str(app), docker_calls=len(calls), disk_image_preserved=True)
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2), flush=True)
        if result["status"] != "passed":
            raise RuntimeError(result.get("error", "Native UI checks failed"))
        passed = True
        if args.keep_open:
            # Keep this exec session and its child alive for native visual/input checks. Creating
            # this fixture-only marker ends the hold; it does not interact with another instance.
            deadline = time.monotonic() + 300
            while not (home / "close").exists() and time.monotonic() < deadline:
                time.sleep(0.1)
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        log.close()
        if passed and not args.keep_open:
            shutil.rmtree(home)


if __name__ == "__main__":
    main()
