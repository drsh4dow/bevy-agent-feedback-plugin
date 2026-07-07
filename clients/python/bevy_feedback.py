#!/usr/bin/env python3
"""Python client for bevy-agent-feedback v2."""
from __future__ import annotations

import json
import os
import socket
import subprocess
import time
from pathlib import Path
from typing import Any, Iterable

PROTOCOL_VERSION = "bevy-agent-feedback/2"


class BevyFeedbackError(RuntimeError):
    """Client, protocol, command, assertion, or OCR error."""


class BevyFeedbackClient:
    """Small JSON-lines client that releases held inputs on close."""

    def __init__(
        self,
        protocol_file: str | os.PathLike[str] | None = None,
        *,
        timeout: float = 10.0,
        transcript_file: str | os.PathLike[str] | None = None,
        tesseract: str | os.PathLike[str] | None = None,
        ocr_language: str = "eng",
        ocr_timeout: float = 5.0,
    ) -> None:
        self.protocol_file = Path(
            protocol_file
            or os.environ.get("BEVY_FEEDBACK_PROTOCOL")
            or "target/agent-feedback/agent-feedback.json"
        )
        self.timeout = timeout
        self.tesseract = str(tesseract or os.environ.get("BEVY_FEEDBACK_TESSERACT") or "tesseract")
        self.ocr_language = ocr_language
        self.ocr_timeout = ocr_timeout
        transcript = transcript_file or os.environ.get("BEVY_FEEDBACK_TRANSCRIPT")
        self._transcript = open(transcript, "a", encoding="utf-8") if transcript else None
        protocol = self._read_protocol()
        self.capture_dir = Path(protocol.get("capture_dir", "."))
        host, port = protocol["socket_addr"].rsplit(":", 1)
        try:
            self._socket = socket.create_connection((host, int(port)), timeout=timeout)
        except ConnectionRefusedError as error:
            raise BevyFeedbackError(f"socket refused at {protocol['socket_addr']}; game probably exited") from error
        self._socket.settimeout(timeout)
        self._stream = self._socket.makefile("rw", encoding="utf-8")
        self._next_id = 1
        self.closed = False

    def __enter__(self) -> "BevyFeedbackClient":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def close(self) -> None:
        """Release agent-held input and close the socket."""
        if self.closed:
            return
        try:
            self.request({"command": "release_all_inputs"})
        except Exception:
            pass
        self.closed = True
        try:
            self._stream.close()
        finally:
            self._socket.close()
            if self._transcript:
                self._transcript.close()

    def request(self, request: dict[str, Any]) -> dict[str, Any]:
        """Send one request and return one response."""
        request = dict(request)
        request.setdefault("id", self._next_id)
        self._next_id += 1
        line = json.dumps(request, separators=(",", ":"))
        if self._transcript:
            self._transcript.write(line + "\n")
            self._transcript.flush()
        self._stream.write(line + "\n")
        self._stream.flush()
        response_line = self._stream.readline()
        if not response_line:
            raise BevyFeedbackError("agent socket closed before response")
        response = json.loads(response_line)
        if response.get("ok") is True:
            return response
        error = response.get("error") or {}
        raise BevyFeedbackError(f"command failed [{error.get('code', 'error')}]: {error.get('message', response)}")

    def replay_jsonl(self, path: str | os.PathLike[str]) -> list[dict[str, Any]]:
        """Replay request-only JSON-lines from disk."""
        responses = []
        with open(path, encoding="utf-8") as handle:
            for line in handle:
                line = line.strip()
                if line:
                    responses.append(self.request(json.loads(line)))
        return responses

    def wait(self, frames: int = 1) -> dict[str, Any]:
        return self.request({"command": "wait", "frames": frames})

    def capture(self) -> Path:
        response = self.request({"command": "capture"})
        return Path(response["result"]["capture"]["path"])

    def window_info(self) -> dict[str, Any]:
        return self.request({"command": "window_info"})

    def cursor_move(self, x: float, y: float) -> dict[str, Any]:
        return self.request({"command": "cursor_move", "x": x, "y": y})

    def key_down(self, key: str) -> dict[str, Any]:
        return self.request({"command": "key_down", "key": key})

    def key_up(self, key: str) -> dict[str, Any]:
        return self.request({"command": "key_up", "key": key})

    def mouse_down(self, button: str = "Left") -> dict[str, Any]:
        return self.request({"command": "mouse_down", "button": button})

    def mouse_up(self, button: str = "Left") -> dict[str, Any]:
        return self.request({"command": "mouse_up", "button": button})

    def click(self, x: float, y: float, button: str = "Left") -> dict[str, Any]:
        return self.request({"command": "click", "x": x, "y": y, "button": button})

    def drag(
        self,
        button: str,
        start: tuple[float, float],
        end: tuple[float, float],
        *,
        steps: int = 10,
        frames: int | None = None,
    ) -> dict[str, Any]:
        return self.request(
            {
                "command": "drag",
                "button": button,
                "from": list(start),
                "to": list(end),
                "steps": steps,
                "frames": frames or steps,
            }
        )

    def scroll(self, lines: float) -> dict[str, Any]:
        return self.request({"command": "scroll", "lines": lines})

    def key_tap(self, key: str) -> dict[str, Any]:
        return self.request({"command": "key_tap", "key": key})

    def key_hold(self, key: str, frames: int) -> dict[str, Any]:
        return self.request({"command": "key_hold", "key": key, "frames": frames})

    def release_all_inputs(self) -> dict[str, Any]:
        return self.request({"command": "release_all_inputs"})

    def shutdown(self) -> dict[str, Any]:
        return self.request({"command": "shutdown"})

    def assert_changed(self, before: str | os.PathLike[str], after: str | os.PathLike[str]) -> None:
        if pixel_diff(before, after) == 0:
            raise BevyFeedbackError(f"screenshots did not change: {before} and {after}")

    def wait_until_changed(self, before: str | os.PathLike[str], *, frames: int = 1, attempts: int = 30) -> Path:
        for _ in range(attempts):
            self.wait(frames)
            after = self.capture()
            if pixel_diff(before, after) > 0:
                return after
        raise BevyFeedbackError("screenshot did not change")

    def wait_until_color(
        self,
        color: tuple[int, int, int],
        region: tuple[int, int, int, int],
        *,
        tolerance: int = 0,
        frames: int = 1,
        attempts: int = 30,
    ) -> Path:
        for _ in range(attempts):
            self.wait(frames)
            capture = self.capture()
            if image_has_color(capture, color, region, tolerance):
                return capture
        raise BevyFeedbackError(f"color did not appear: {color}")

    def ocr_image(self, path: str | os.PathLike[str]) -> str:
        return _run_tesseract(self.tesseract, self.ocr_language, self.ocr_timeout, Path(path))

    def ocr_region(self, path: str | os.PathLike[str], region: tuple[int, int, int, int]) -> str:
        try:
            from PIL import Image
        except ImportError as error:
            raise BevyFeedbackError("Pillow is required for OCR region crops") from error
        image = Image.open(path)
        x, y, width, height = region
        cropped = image.crop((x, y, x + width, y + height))
        temp = Path(os.environ.get("TMPDIR", "/tmp")) / f"bevy-feedback-ocr-{os.getpid()}-{time.time_ns()}.png"
        cropped.save(temp)
        try:
            return self.ocr_image(temp)
        finally:
            temp.unlink(missing_ok=True)

    def assert_text(self, path: str | os.PathLike[str], expected: str) -> None:
        text = _normalize_text(self.ocr_image(path))
        expected = _normalize_text(expected)
        if expected not in text:
            raise BevyFeedbackError(f"OCR text did not contain {expected!r}: {text}")

    def wait_until_text(self, expected: str, *, frames: int = 1, attempts: int = 30) -> Path:
        for _ in range(attempts):
            self.wait(frames)
            capture = self.capture()
            try:
                self.assert_text(capture, expected)
                return capture
            except BevyFeedbackError as error:
                if not str(error).startswith("OCR text did not contain"):
                    raise
        raise BevyFeedbackError(f"text did not appear: {expected}")

    def _read_protocol(self) -> dict[str, Any]:
        with open(self.protocol_file, encoding="utf-8") as handle:
            protocol = json.load(handle)
        version = protocol.get("protocol")
        if version != PROTOCOL_VERSION:
            raise BevyFeedbackError(f"unsupported protocol {version!r}; expected {PROTOCOL_VERSION}")
        pid = int(protocol.get("pid", 0))
        if pid <= 0 or not _process_alive(pid):
            raise BevyFeedbackError(f"protocol stale: process {pid} is not alive")
        heartbeat_file = Path(protocol["heartbeat_file"])
        try:
            heartbeat_ms = int(heartbeat_file.read_text(encoding="utf-8").strip())
        except OSError as error:
            raise BevyFeedbackError(f"protocol stale: failed to read heartbeat {heartbeat_file}: {error}") from error
        age = int(time.time() * 1000) - heartbeat_ms
        stale_after = int(protocol["stale_after_ms"])
        if age > stale_after:
            raise BevyFeedbackError(f"protocol stale: heartbeat is {age}ms old, stale after {stale_after}ms")
        return protocol


def pixel_diff(a: str | os.PathLike[str], b: str | os.PathLike[str], region: tuple[int, int, int, int] | None = None) -> int:
    try:
        from PIL import Image
    except ImportError as error:
        raise BevyFeedbackError("Pillow is required for image assertions") from error
    image_a = Image.open(a).convert("RGBA")
    image_b = Image.open(b).convert("RGBA")
    if image_a.size != image_b.size:
        raise BevyFeedbackError(f"image dimensions differ: {image_a.size} vs {image_b.size}")
    x, y, width, height = region or (0, 0, image_a.width, image_a.height)
    changed = 0
    for py in range(y, y + height):
        for px in range(x, x + width):
            changed += image_a.getpixel((px, py)) != image_b.getpixel((px, py))
    return changed


def region_diff(a: str | os.PathLike[str], b: str | os.PathLike[str], region: tuple[int, int, int, int]) -> int:
    """Count differing pixels inside a region."""
    return pixel_diff(a, b, region)


def image_has_color(
    path: str | os.PathLike[str],
    color: tuple[int, int, int],
    region: tuple[int, int, int, int],
    tolerance: int = 0,
) -> bool:
    try:
        from PIL import Image
    except ImportError as error:
        raise BevyFeedbackError("Pillow is required for image assertions") from error
    image = Image.open(path).convert("RGB")
    x, y, width, height = region
    for py in range(y, y + height):
        for px in range(x, x + width):
            pixel = image.getpixel((px, py))
            if all(abs(pixel[index] - color[index]) <= tolerance for index in range(3)):
                return True
    return False


def _run_tesseract(tesseract: str, language: str, timeout: float, path: Path) -> str:
    try:
        output = subprocess.run(
            [tesseract, str(path), "stdout", "-l", language],
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except FileNotFoundError as error:
        raise BevyFeedbackError(f"tesseract unavailable at {tesseract}: {error}") from error
    except subprocess.TimeoutExpired as error:
        raise BevyFeedbackError(f"tesseract timed out after {timeout}s") from error
    if output.returncode != 0:
        raise BevyFeedbackError(output.stderr.strip() or "tesseract failed")
    return output.stdout


def _normalize_text(value: str) -> str:
    return " ".join(value.split()).lower()


def _process_alive(pid: int) -> bool:
    if os.name == "posix":
        try:
            os.kill(pid, 0)
            return True
        except OSError:
            return False
    return True


def drive_stdio(protocol_file: str | os.PathLike[str], lines: Iterable[str]) -> None:
    """Compatibility helper: stdin JSON-lines to stdout JSON-lines."""
    with BevyFeedbackClient(protocol_file) as client:
        for line_number, line in enumerate(lines, 1):
            line = line.strip()
            if not line:
                continue
            command = json.loads(line)
            command.setdefault("id", line_number)
            print(json.dumps(client.request(command), separators=(",", ":")), flush=True)
