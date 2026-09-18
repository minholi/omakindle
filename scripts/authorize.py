# /// script
# requires-python = ">=3.11"
# dependencies = ["playwright>=1.55"]
# ///
"""Open the default browser, let the user sign in to Amazon, and store the
session with the plugin backend.

Captures the read.amazon.com cookies and the deviceToken the Kindle web
reader sends when it registers itself. If the token cannot be observed it
stores the cookies only and reports that the token is still needed.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
import urllib.parse
from pathlib import Path

REGION_SUFFIXES = {
    "us": "com", "uk": "co.uk", "de": "de", "fr": "fr", "it": "it", "es": "es",
    "jp": "co.jp", "ca": "ca", "au": "com.au", "in": "in", "br": "com.br",
    "mx": "com.mx", "nl": "nl",
}
REQUIRED_COOKIES = ("ubid-main", "at-main", "x-main", "session-id")

CHROMIUM_FALLBACKS = [
    ("channel", "chrome", "Google Chrome"),
    ("channel", "msedge", "Microsoft Edge"),
    ("path", "/opt/microsoft/msedge/msedge", "Microsoft Edge"),
    ("path", "/usr/bin/chromium", "Chromium"),
    ("path", "/usr/bin/google-chrome-stable", "Google Chrome"),
    ("path", "/usr/bin/brave", "Brave"),
]


def emit(payload: dict) -> None:
    print(json.dumps(payload), flush=True)


def default_browser_desktop() -> str:
    try:
        result = subprocess.run(
            ["xdg-settings", "get", "default-web-browser"],
            capture_output=True, text=True, timeout=10,
        )
        return result.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def desktop_exec(desktop: str) -> str:
    if not desktop:
        return ""
    bases = [
        Path.home() / ".local/share/applications",
        Path("/usr/local/share/applications"),
        Path("/usr/share/applications"),
    ]
    for base in bases:
        candidate = base / desktop
        if not candidate.is_file():
            continue
        try:
            text = candidate.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        match = re.search(r"^Exec=\"?([^\"\n]+?)\"?\s*(?:%[fFuUdDnNickvm]|$)", text, re.M)
        if match:
            return match.group(1).strip()
    return ""


def launch_config(binary: str) -> dict | None:
    name = Path(binary).name.lower() if binary else ""
    if not name:
        return None
    if "edge" in name:
        return {"channel": "msedge", "label": "Microsoft Edge"}
    if "chromium" in name:
        return {"executable_path": binary, "label": "Chromium"}
    if "chrome" in name:
        return {"channel": "chrome", "label": "Google Chrome"}
    if "brave" in name:
        return {"executable_path": binary, "label": "Brave"}
    if "vivaldi" in name:
        return {"executable_path": binary, "label": "Vivaldi"}
    if "opera" in name:
        return {"executable_path": binary, "label": "Opera"}
    return {"executable_path": binary, "label": Path(binary).name}


def resolve_browser(override: str) -> dict:
    if override:
        if override in ("chrome", "msedge", "chromium"):
            return {"channel": override, "label": override}
        return {"executable_path": override, "label": Path(override).name}

    binary = desktop_exec(default_browser_desktop())
    config = launch_config(binary)
    if config:
        return config

    for kind, value, label in CHROMIUM_FALLBACKS:
        if kind == "channel" and value == "chrome" and shutil.which("google-chrome") is None:
            continue
        if kind == "channel" and value == "msedge" and shutil.which("microsoft-edge-stable") is None:
            continue
        if kind == "path" and not Path(value).is_file():
            continue
        if kind == "channel":
            return {"channel": value, "label": label}
        return {"executable_path": value, "label": label}

    raise RuntimeError(
        "no Chromium-based browser found; install one or pass --browser"
    )


def cookie_header(cookies) -> str:
    parts = []
    for cookie in cookies:
        if "amazon" not in cookie.get("domain", ""):
            continue
        parts.append(f"{cookie['name']}={cookie['value']}")
    return "; ".join(parts)


def has_required_cookies(cookies) -> bool:
    names = {cookie["name"] for cookie in cookies if "amazon" in cookie.get("domain", "")}
    return all(name in names for name in REQUIRED_COOKIES)


def device_serial(url: str) -> str:
    query = urllib.parse.urlparse(url).query
    value = urllib.parse.parse_qs(query).get("serialNumber", [""])[0]
    return value


def rpc(socket_path: str, command: str, **fields) -> dict | None:
    payload = {"v": 1, "id": 1, "command": command, **fields}
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(180)
        client.connect(socket_path)
        client.sendall(json.dumps(payload).encode() + b"\n")
        buffer = b""
        while True:
            while b"\n" not in buffer:
                chunk = client.recv(65536)
                if not chunk:
                    return None
                buffer += chunk
            line, buffer = buffer.split(b"\n", 1)
            message = json.loads(line)
            if message.get("type") == "response" and message.get("id") == 1:
                return message


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--region", default="us")
    parser.add_argument("--browser", default="")
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--socket", default=os.path.join(
        os.environ.get("XDG_RUNTIME_DIR", "/tmp"), "omakindle", "backend.sock"))
    args = parser.parse_args()

    suffix = REGION_SUFFIXES.get(args.region.lower(), args.region.lower())
    base = f"https://read.amazon.{suffix}"
    profile_dir = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "omakindle" / "browser"
    profile_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(profile_dir, 0o700)

    try:
        config = resolve_browser(args.browser)
    except RuntimeError as error:
        emit({"ok": False, "error": str(error)})
        return 1
    emit({"status": "launching", "browser": config["label"]})

    from playwright.sync_api import sync_playwright

    captured_tokens: list[str] = []
    with sync_playwright() as playwright:
        launch_args = ["--no-first-run", "--no-default-browser-check"]
        launch_kwargs = dict(
            headless=False,
            args=launch_args,
        )
        if "channel" in config:
            launch_kwargs["channel"] = config["channel"]
        else:
            launch_kwargs["executable_path"] = config["executable_path"]
        context = playwright.chromium.launch_persistent_context(
            str(profile_dir), **launch_kwargs)
        try:
            page = context.pages[0] if context.pages else context.new_page()
            page.on("request", lambda request: captured_tokens.append(request.url)
                    if "getDeviceToken" in request.url else None)

            page.goto(f"{base}/kindle-library", wait_until="domcontentloaded", timeout=60000)
            emit({"status": "waiting-login", "browser": config["label"]})

            deadline = time.time() + args.timeout
            signed_in = False
            while time.time() < deadline:
                cookies = context.cookies()
                if has_required_cookies(cookies):
                    signed_in = True
                    break
                time.sleep(2)

            if not signed_in:
                emit({"ok": False, "error": "timed out waiting for Amazon sign-in"})
                return 1
            emit({"status": "signed-in"})
            time.sleep(2)

            try:
                page.goto(f"{base}/kindle-library", wait_until="domcontentloaded", timeout=60000)
            except Exception:
                pass

            deadline = time.time() + 20
            while time.time() < deadline and not captured_tokens:
                time.sleep(1)

            if not captured_tokens:
                emit({"status": "triggering-token"})
                try:
                    session_id = next(
                        cookie["value"] for cookie in context.cookies()
                        if cookie["name"] == "session-id" and "amazon" in cookie["domain"])
                    response = context.request.get(
                        f"{base}/kindle-library/search?query=&libraryType=BOOKS&sortType=recency&querySize=5",
                        headers={"accept": "*/*", "x-amzn-sessionid": session_id},
                        timeout=30000)
                    items = response.json().get("itemsList", [])
                    asin = items[0]["asin"] if items else ""
                except Exception:
                    asin = ""
                if asin:
                    try:
                        page.goto(f"{base}/?asin={asin}", wait_until="domcontentloaded", timeout=60000)
                    except Exception:
                        pass
                    deadline = time.time() + 25
                    while time.time() < deadline and not captured_tokens:
                        time.sleep(1)

            token = ""
            for url in captured_tokens:
                token = device_serial(url)
                if token:
                    break

            cookies = context.cookies()
            header = cookie_header(cookies)
            if not header:
                emit({"ok": False, "error": "no Amazon cookies were captured"})
                return 1

            if token:
                emit({"status": "saving", "hasToken": True})
                response = rpc(args.socket, "set_credentials",
                               cookies=header, deviceToken=token, region=args.region)
            else:
                emit({"status": "saving", "hasToken": False})
                response = rpc(args.socket, "store_cookies",
                               cookies=header, region=args.region)

            if response is None or not response.get("ok"):
                error = "backend did not accept the session"
                if response and response.get("error"):
                    error = response["error"]
                emit({"ok": False, "error": error, "hasToken": bool(token)})
                return 1

            emit({"ok": True, "hasToken": bool(token), "browser": config["label"]})
            return 0
        finally:
            try:
                context.close()
            except Exception:
                pass


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        emit({"ok": False, "error": "cancelled"})
        sys.exit(130)
    except Exception as error:  # pragma: no cover - top level guard
        emit({"ok": False, "error": f"{type(error).__name__}: {error}"})
        sys.exit(1)
