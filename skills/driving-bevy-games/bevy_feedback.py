#!/usr/bin/env python3
"""Python client for bevy-agent-feedback 0.5."""
from __future__ import annotations

import json
import math
import os
import socket
import subprocess
import time
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation, ROUND_HALF_EVEN
from pathlib import Path
from typing import Any, Callable, Iterable, NoReturn, Sequence

PROTOCOL_VERSION = "bevy-agent-feedback/0.5"
DEFAULT_MAX_WAIT_FRAMES = 300
DEFAULT_MAX_ABORT_PREDICATES = 16
DEFAULT_MAX_TIME_ADVANCE_STEPS = 600
DEFAULT_MAX_TIME_ADVANCE_SECONDS = 10.0
NANOSECONDS_PER_SECOND = 1_000_000_000
MAX_CLIENT_CHUNKS = 4096


class BevyFeedbackError(RuntimeError):
    """Client, protocol, command, assertion, or OCR error."""

    def __init__(
        self,
        message: str,
        *,
        code: str | None = None,
        context: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.context = context

    def attach_failure_capture(self, capture: dict[str, Any]) -> None:
        context = dict(self.context or {})
        context["failure_capture"] = dict(capture)
        self.context = context
        metadata = json.dumps(capture, sort_keys=True, separators=(",", ":"))
        self.args = (f"{self.args[0]}; failure_capture={metadata}",)


@dataclass(frozen=True)
class BevyFeedbackCapabilities:
    """Immutable capabilities advertised by the running game."""

    max_wait_frames: int
    max_abort_predicates: int
    deterministic_time: bool
    max_time_advance_steps: int
    max_time_advance_seconds: float


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
        self.last_capture: Path | None = None
        self.last_capture_info: dict[str, Any] | None = None
        self.last_observation: dict[str, Any] | None = None
        self.last_error_context: dict[str, Any] | None = None
        transcript = transcript_file or os.environ.get("BEVY_FEEDBACK_TRANSCRIPT")
        self._transcript = open(transcript, "a", encoding="utf-8") if transcript else None
        protocol = self._read_protocol()
        self.capture_dir = Path(protocol.get("capture_dir", "."))
        commands = protocol.get("commands")
        additive_timing = isinstance(commands, dict) and "advance_time" in commands
        self._timing_advertised = additive_timing
        self.capabilities = BevyFeedbackCapabilities(
            max_wait_frames=_protocol_positive_int(
                protocol, "max_wait_frames", DEFAULT_MAX_WAIT_FRAMES, additive_timing
            ),
            max_abort_predicates=_protocol_positive_int(
                protocol,
                "max_abort_predicates",
                DEFAULT_MAX_ABORT_PREDICATES,
                False,
            ),
            deterministic_time=_protocol_bool(
                protocol, "deterministic_time", False, additive_timing
            ),
            max_time_advance_steps=_protocol_positive_int(
                protocol,
                "max_time_advance_steps",
                DEFAULT_MAX_TIME_ADVANCE_STEPS,
                additive_timing,
            ),
            max_time_advance_seconds=_protocol_positive_float(
                protocol,
                "max_time_advance_seconds",
                DEFAULT_MAX_TIME_ADVANCE_SECONDS,
                additive_timing,
            ),
        )
        host, port = protocol["socket_addr"].rsplit(":", 1)
        try:
            self._socket = socket.create_connection((host, int(port)), timeout=timeout)
        except ConnectionRefusedError as error:
            raise BevyFeedbackError(f"socket refused at {protocol['socket_addr']}; game probably exited") from error
        self._socket.settimeout(timeout)
        self._stream = self._socket.makefile("rw", encoding="utf-8")
        self._next_id = 1
        self.closed = False

    @property
    def max_wait_frames(self) -> int:
        return self.capabilities.max_wait_frames

    @property
    def max_abort_predicates(self) -> int:
        return self.capabilities.max_abort_predicates

    @property
    def deterministic_time(self) -> bool:
        return self.capabilities.deterministic_time

    @property
    def max_time_advance_steps(self) -> int:
        return self.capabilities.max_time_advance_steps

    @property
    def max_time_advance_seconds(self) -> float:
        return self.capabilities.max_time_advance_seconds

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
        ts_ms = int(time.time() * 1000)
        started = time.monotonic()
        self._stream.write(line + "\n")
        self._stream.flush()
        try:
            response_line = self._stream.readline()
        except socket.timeout as error:
            message = self._with_last_capture(f"agent request timed out after {self.timeout}s")
            raise BevyFeedbackError(message) from error
        if not response_line:
            raise BevyFeedbackError("agent socket closed before response")
        response = json.loads(response_line)
        self._write_transcript(ts_ms, int((time.monotonic() - started) * 1000), request, response)
        if response.get("ok") is True:
            self._retain_response_context(response)
            return response
        error = response.get("error") or {}
        context = error.get("context")
        self.last_error_context = dict(context) if isinstance(context, dict) else None
        if self.last_error_context is not None:
            self._retain_context(self.last_error_context)
        message = error.get("message", response)
        if error.get("code") == "timeout":
            message = self._with_last_capture(str(message))
        if self.last_error_context is not None:
            message = (
                f"{message}; context="
                f"{json.dumps(self.last_error_context, sort_keys=True, separators=(',', ':'))}"
            )
        raise BevyFeedbackError(
            f"command failed [{error.get('code', 'error')}]: {message}",
            code=error.get("code") if isinstance(error.get("code"), str) else None,
            context=self.last_error_context,
        )

    def replay_jsonl(self, path: str | os.PathLike[str]) -> list[dict[str, Any]]:
        """Replay request-only or transcript-envelope JSON-lines from disk."""
        responses = []
        with open(path, encoding="utf-8") as handle:
            lines = list(handle)
        for line in lines:
            line = line.strip()
            if line:
                value = json.loads(line)
                request = value.get("request", value) if isinstance(value, dict) else value
                responses.append(self.request(request))
        return responses

    def wait_frames(self, frames: int = 1) -> dict[str, Any]:
        """Wait for a positive number of app updates in one bounded request."""
        frames = _positive_int("frames", frames)
        _wait_limit("frames", frames, self.max_wait_frames)
        return self.request({"command": "wait", "frames": frames})

    def wait_seconds(
        self, seconds: float, *, max_frames: int | None = None
    ) -> dict[str, Any]:
        if not getattr(self, "_timing_advertised", True):
            raise BevyFeedbackError("wait_seconds is not advertised by this running game")
        seconds = _positive_seconds("seconds", seconds)
        request: dict[str, Any] = {"command": "wait_seconds", "seconds": seconds}
        if max_frames is not None:
            max_frames = _positive_int("max_frames", max_frames)
            _wait_limit("max_frames", max_frames, self.max_wait_frames)
            request["max_frames"] = max_frames
        return self.request(request)

    def advance_time(
        self, seconds: float, *, step_seconds: float | None = None
    ) -> list[dict[str, Any]]:
        if not getattr(self, "_timing_advertised", True):
            raise BevyFeedbackError("advance_time is not advertised by this running game")
        total_ns = _duration_nanoseconds("seconds", seconds)
        cap_ns = _duration_nanoseconds(
            "max_time_advance_seconds", self.max_time_advance_seconds
        )
        step_ns = (
            _duration_nanoseconds("step_seconds", step_seconds)
            if step_seconds is not None
            else None
        )
        if step_ns is None:
            if total_ns > cap_ns:
                raise BevyFeedbackError(
                    "advance_time requires explicit step_seconds when chunking; "
                    "the server's default nominal step is not discoverable"
                )
            chunks = [total_ns]
        else:
            chunk_steps = min(
                self.max_time_advance_steps,
                cap_ns // step_ns,
            )
            if chunk_steps < 1:
                raise BevyFeedbackError(
                    "step_seconds exceeds advertised max_time_advance_seconds"
                )
            chunk_ns = chunk_steps * step_ns
            chunk_count = (total_ns + chunk_ns - 1) // chunk_ns
            if chunk_count > MAX_CLIENT_CHUNKS:
                raise BevyFeedbackError(
                    f"advance_time requires {chunk_count} chunks; maximum is {MAX_CLIENT_CHUNKS}"
                )
            chunks = []
            remaining_ns = total_ns
            for _ in range(chunk_count):
                current_ns = min(remaining_ns, chunk_ns)
                chunks.append(current_ns)
                remaining_ns -= current_ns
        responses = []
        for chunk_ns in chunks:
            request = {
                "command": "advance_time",
                "seconds": chunk_ns / NANOSECONDS_PER_SECOND,
            }
            if step_ns is not None:
                request["step_seconds"] = step_ns / NANOSECONDS_PER_SECOND
            responses.append(self.request(request))
        return responses

    def capture(self, label: str | None = None) -> Path:
        request: dict[str, Any] = {"command": "capture"}
        if label is not None:
            request["label"] = label
        return self._record_capture(self.request(request))

    def capture_after_frames(self, frames: int, label: str | None = None) -> Path:
        frames = _bounded_frames("frames", frames, self.max_wait_frames)
        request: dict[str, Any] = {"command": "capture_after_frames", "frames": frames}
        if label is not None:
            request["label"] = label
        return self._record_capture(self.request(request))

    def wait_until_first_capture(self) -> Path:
        """Return the first completion-confirmed delayed capture."""
        return self.capture_after_frames(1)

    def window_info(self) -> dict[str, Any]:
        return self.request({"command": "window_info"})

    def window_center(self) -> tuple[float, float]:
        width, height = self._logical_window_size()
        return (width / 2.0, height / 2.0)

    def point(self, frac_x: float, frac_y: float) -> tuple[float, float]:
        if not (0.0 <= frac_x < 1.0 and 0.0 <= frac_y < 1.0):
            raise BevyFeedbackError(
                "point fractions must satisfy 0.0 <= frac_x < 1.0 and 0.0 <= frac_y < 1.0"
            )
        width, height = self._logical_window_size()
        return (width * frac_x, height * frac_y)

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

    def target_info(
        self,
        target: dict[str, str],
        *,
        kind: str | None = None,
        camera: str | None = None,
    ) -> dict[str, Any]:
        request = _target_request("target_info", target, kind, camera)
        return self.request(request)

    def wait_for_target(
        self,
        target: dict[str, str],
        *,
        kind: str | None = None,
        camera: str | None = None,
        max_frames: int | None = None,
    ) -> dict[str, Any]:
        predicate = _target_predicate("target_exists", target, kind, camera)
        return self.wait_for(predicate, max_frames=max_frames)

    def wait_for_target_absent(
        self,
        target: dict[str, str],
        *,
        kind: str | None = None,
        camera: str | None = None,
        max_frames: int | None = None,
    ) -> dict[str, Any]:
        predicate = _target_predicate("target_absent", target, kind, camera)
        return self.wait_for(predicate, max_frames=max_frames)

    def click_target(
        self,
        target: dict[str, str],
        *,
        kind: str | None = None,
        camera: str | None = None,
        button: str | None = None,
        frames: int | None = None,
    ) -> dict[str, Any]:
        request = _target_request("click_target", target, kind, camera)
        if button is not None:
            request["button"] = button
        if frames is not None:
            request["frames"] = _bounded_frames("frames", frames, self.max_wait_frames)
        return self.request(request)

    def click_named(
        self,
        name: str,
        *,
        kind: str | None = None,
        camera: str | None = None,
        button: str | None = None,
        frames: int | None = None,
    ) -> dict[str, Any]:
        return self.click_target(
            {"name": name}, kind=kind, camera=camera, button=button, frames=frames
        )

    def click_accessibility_label(
        self,
        label: str,
        *,
        kind: str | None = None,
        camera: str | None = None,
        button: str | None = None,
        frames: int | None = None,
    ) -> dict[str, Any]:
        return self.click_target(
            {"accessibility_label": label},
            kind=kind,
            camera=camera,
            button=button,
            frames=frames,
        )

    def click_marker(
        self,
        marker: str,
        *,
        kind: str | None = None,
        camera: str | None = None,
        button: str | None = None,
        frames: int | None = None,
    ) -> dict[str, Any]:
        return self.click_target(
            {"marker": marker}, kind=kind, camera=camera, button=button, frames=frames
        )

    def resource_info(
        self, resource: str | None = None, field: str | None = None
    ) -> dict[str, Any]:
        request: dict[str, Any] = {"command": "resource_info"}
        if resource is not None:
            request["resource"] = resource
        if field is not None:
            request["field"] = field
        return self.request(request)

    def read_resource_field(self, resource: str, field: str) -> Any:
        response = self.resource_info(resource, field)
        details = _response_details(response)
        if "value" not in details:
            raise BevyFeedbackError(f"resource_info response missing field value: {response}")
        return details["value"]

    def evaluate_predicate(self, predicate: dict[str, Any]) -> dict[str, Any]:
        response = self.request(
            {"command": "evaluate_predicate", "predicate": dict(predicate)}
        )
        return self._record_observation(response)

    def wait_for(
        self,
        predicate: dict[str, Any],
        *,
        abort_predicates: Sequence[dict[str, Any]] = (),
        max_frames: int | None = None,
    ) -> dict[str, Any]:
        abort_predicates = _bounded_abort_predicates(
            abort_predicates, self.max_abort_predicates
        )
        request: dict[str, Any] = {
            "command": "wait_for",
            "predicate": dict(predicate),
        }
        if abort_predicates:
            request["abort_predicates"] = [dict(item) for item in abort_predicates]
        if max_frames is not None:
            max_frames = _positive_int("max_frames", max_frames)
            _wait_limit("max_frames", max_frames, self.max_wait_frames)
            request["max_frames"] = max_frames
        try:
            response = self.request(request)
        except BevyFeedbackError as error:
            if error.code in ("predicate_timeout", "predicate_aborted"):
                try:
                    self.capture("semantic-wait-failure")
                except Exception:
                    pass
                else:
                    if self.last_capture_info is not None:
                        error.attach_failure_capture(self.last_capture_info)
            raise
        return self._record_observation(response)

    def wait_for_state(
        self,
        state: str,
        value: Any,
        *,
        abort_values: Sequence[Any] = (),
        max_frames: int | None = None,
    ) -> dict[str, Any]:
        if not isinstance(abort_values, Sequence) or isinstance(
            abort_values, (str, bytes)
        ):
            raise BevyFeedbackError("abort_values must be a sequence")
        if len(abort_values) > self.max_abort_predicates:
            raise BevyFeedbackError(
                f"abort_values has {len(abort_values)} items, but server supports "
                f"{self.max_abort_predicates}; reduce abort values or configure "
                "separate explicit waits"
            )
        return self.wait_for(
            {"type": "state_equals", "state": state, "value": value},
            abort_predicates=[
                {"type": "state_equals", "state": state, "value": abort}
                for abort in abort_values
            ],
            max_frames=max_frames,
        )

    def wait_for_resource(
        self,
        resource: str,
        field: str,
        operator: str,
        value: Any,
        *,
        max_frames: int | None = None,
    ) -> dict[str, Any]:
        return self.wait_for(
            {
                "type": "resource_field",
                "resource": resource,
                "field": field,
                "operator": operator,
                "value": value,
            },
            max_frames=max_frames,
        )

    def wait_for_marker_count(
        self,
        marker: str,
        *,
        min_count: int | None = None,
        max_count: int | None = None,
        max_frames: int | None = None,
    ) -> dict[str, Any]:
        predicate = _marker_predicate(marker, min_count, max_count)
        return self.wait_for(predicate, max_frames=max_frames)

    def wait_for_marker_present(
        self, marker: str, *, max_frames: int | None = None
    ) -> dict[str, Any]:
        return self.wait_for_marker_count(
            marker, min_count=1, max_frames=max_frames
        )

    def wait_for_marker_absent(
        self, marker: str, *, max_frames: int | None = None
    ) -> dict[str, Any]:
        return self.wait_for_marker_count(
            marker, max_count=0, max_frames=max_frames
        )

    def assert_state(self, state: str, value: Any) -> None:
        self._assert_predicate(
            {"type": "state_equals", "state": state, "value": value}
        )

    def assert_resource(
        self, resource: str, field: str, operator: str, value: Any
    ) -> None:
        self._assert_predicate(
            {
                "type": "resource_field",
                "resource": resource,
                "field": field,
                "operator": operator,
                "value": value,
            }
        )

    def assert_marker_count(
        self,
        marker: str,
        *,
        min_count: int | None = None,
        max_count: int | None = None,
    ) -> None:
        self._assert_predicate(_marker_predicate(marker, min_count, max_count))

    def assert_marker_present(self, marker: str) -> None:
        self.assert_marker_count(marker, min_count=1)

    def assert_marker_absent(self, marker: str) -> None:
        self.assert_marker_count(marker, max_count=0)

    def assert_target_exists(
        self,
        target: dict[str, str],
        *,
        kind: str | None = None,
        camera: str | None = None,
    ) -> None:
        self._assert_predicate(
            _target_predicate("target_exists", target, kind, camera)
        )

    def assert_target_absent(
        self,
        target: dict[str, str],
        *,
        kind: str | None = None,
        camera: str | None = None,
    ) -> None:
        self._assert_predicate(
            _target_predicate("target_absent", target, kind, camera)
        )

    def assert_changed(
        self,
        before: str | os.PathLike[str],
        after: str | os.PathLike[str],
        *,
        min_pixels: int = 1,
        include: tuple[int, int, int, int] | None = None,
        masks: Sequence[tuple[int, int, int, int]] = (),
    ) -> None:
        changed = pixel_diff(before, after, include=include, masks=masks)
        if changed < min_pixels:
            raise BevyFeedbackError(
                f"screenshots changed {changed} pixels, expected at least {min_pixels}: {before} and {after}"
            )

    def assert_region_changed(
        self,
        before: str | os.PathLike[str],
        after: str | os.PathLike[str],
        region: tuple[int, int, int, int],
        *,
        min_pixels: int = 1,
        masks: Sequence[tuple[int, int, int, int]] = (),
    ) -> None:
        changed = region_diff(before, after, region, masks=masks)
        if changed < min_pixels:
            raise BevyFeedbackError(
                f"region {region} changed {changed} pixels, expected at least {min_pixels}: {before} and {after}"
            )

    def assert_color_present(
        self,
        path: str | os.PathLike[str],
        rgb: tuple[int, int, int],
        region: tuple[int, int, int, int] | None = None,
        *,
        tolerance: int = 0,
        min_pixels: int = 1,
    ) -> None:
        found = color_pixel_count(path, rgb, region, tolerance)
        if found < min_pixels:
            raise BevyFeedbackError(
                f"color {rgb} found {found} pixels, expected at least {min_pixels}: {path}"
            )

    def wait_until_changed(
        self,
        before: str | os.PathLike[str],
        *,
        frames: int = 1,
        attempts: int = 30,
        label: str | None = None,
        include: tuple[int, int, int, int] | None = None,
        masks: Sequence[tuple[int, int, int, int]] = (),
    ) -> Path:
        masks = _bounded_masks(masks)
        attempts = _bounded_attempts(attempts)
        frames = _bounded_frames("frames", frames, self.max_wait_frames)
        for _ in range(attempts):
            after = self.capture_after_frames(frames, label)
            if pixel_diff(before, after, include=include, masks=masks) > 0:
                return after
        raise BevyFeedbackError("screenshot did not change")

    def wait_until_stable(
        self,
        *,
        frames: int = 10,
        attempts: int = 30,
        stable: int = 2,
        label: str | None = None,
        include: tuple[int, int, int, int] | None = None,
        masks: Sequence[tuple[int, int, int, int]] = (),
    ) -> Path:
        """Wait until `stable` consecutive captures are pixel-identical."""
        attempts = _bounded_attempts(attempts)
        stable = _bounded_attempts(stable)
        frames = _bounded_frames("frames", frames, self.max_wait_frames)
        masks = _bounded_masks(masks)
        previous = self.capture(label)
        streak = 0
        for _ in range(attempts):
            current = self.capture_after_frames(frames, label)
            try:
                changed = pixel_diff(previous, current, include=include, masks=masks)
            except BevyFeedbackError as error:
                if "image dimensions differ" not in str(error):
                    raise
                changed = 1
            if changed == 0:
                streak += 1
                if streak >= stable:
                    return current
            else:
                streak = 0
            previous = current
        raise BevyFeedbackError(
            f"screen did not stabilize: attempts={attempts}, frames={frames}, "
            f"include={include}, masks={masks}, last_capture={self.last_capture_info}"
        )

    def wait_until_color(
        self,
        color: tuple[int, int, int],
        region: tuple[int, int, int, int],
        *,
        tolerance: int = 0,
        frames: int = 1,
        attempts: int = 30,
        label: str | None = None,
    ) -> Path:
        attempts = _bounded_attempts(attempts)
        frames = _bounded_frames("frames", frames, self.max_wait_frames)
        for _ in range(attempts):
            capture = self.capture_after_frames(frames, label)
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

    def wait_until_text(
        self,
        expected: str,
        *,
        frames: int = 1,
        attempts: int = 30,
        label: str | None = None,
    ) -> Path:
        attempts = _bounded_attempts(attempts)
        frames = _bounded_frames("frames", frames, self.max_wait_frames)
        for _ in range(attempts):
            capture = self.capture_after_frames(frames, label)
            try:
                self.assert_text(capture, expected)
                return capture
            except BevyFeedbackError as error:
                if not str(error).startswith("OCR text did not contain"):
                    raise
        raise BevyFeedbackError(f"text did not appear: {expected}")

    def _record_observation(self, response: dict[str, Any]) -> dict[str, Any]:
        observation = _response_details(response)
        self.last_observation = dict(observation)
        return self.last_observation

    def _assert_predicate(self, predicate: dict[str, Any]) -> None:
        observation = self.evaluate_predicate(predicate)
        if observation.get("outcome") != "matched":
            raise BevyFeedbackError(
                "predicate assertion failed: "
                f"{json.dumps(observation, sort_keys=True, separators=(',', ':'))}"
            )

    def _retain_response_context(self, response: dict[str, Any]) -> None:
        result = response.get("result")
        if not isinstance(result, dict):
            return
        latest_capture = result.get("latest_capture")
        if isinstance(latest_capture, dict):
            self._retain_capture_info(latest_capture)
        details = result.get("details")
        if isinstance(details, dict) and isinstance(details.get("predicate"), dict):
            self.last_observation = dict(details)

    def _retain_context(self, context: dict[str, Any]) -> None:
        latest_capture = context.get("latest_capture")
        if isinstance(latest_capture, dict):
            self._retain_capture_info(latest_capture)
        observation = context.get("observed_predicate")
        if isinstance(observation, dict):
            self.last_observation = dict(observation)

    def _retain_capture_info(self, capture: dict[str, Any]) -> None:
        self.last_capture_info = dict(capture)
        path = capture.get("path")
        if isinstance(path, str):
            self.last_capture = Path(path)

    def _write_transcript(
        self,
        ts_ms: int,
        duration_ms: int,
        request: dict[str, Any],
        response: dict[str, Any],
    ) -> None:
        if self._transcript:
            self._transcript.write(
                json.dumps(
                    {
                        "ts_ms": ts_ms,
                        "duration_ms": duration_ms,
                        "request": request,
                        "response": response,
                    },
                    separators=(",", ":"),
                )
                + "\n"
            )
            self._transcript.flush()

    def _logical_window_size(self) -> tuple[float, float]:
        response = self.window_info()
        result = response.get("result")
        window = result.get("window") if isinstance(result, dict) else None
        width = window.get("logical_width") if isinstance(window, dict) else None
        height = window.get("logical_height") if isinstance(window, dict) else None
        if not isinstance(width, (int, float)) or not isinstance(height, (int, float)):
            raise BevyFeedbackError(f"window_info response missing logical window dimensions: {response}")
        return (float(width), float(height))

    def _record_capture(self, response: dict[str, Any]) -> Path:
        capture = response["result"]["capture"]
        if not isinstance(capture, dict):
            raise BevyFeedbackError(f"capture response missing metadata: {response}")
        path = capture.get("path")
        if not isinstance(path, str):
            raise BevyFeedbackError(f"capture response missing path: {response}")
        self._retain_capture_info(capture)
        if self.last_capture is None:
            raise BevyFeedbackError(f"capture response missing path: {response}")
        return self.last_capture

    def _with_last_capture(self, message: str) -> str:
        if self.last_capture:
            return f"{message}; last captured frame: {self.last_capture}"
        return message

    def _read_protocol(self) -> dict[str, Any]:
        try:
            with open(self.protocol_file, encoding="utf-8") as handle:
                protocol = json.load(handle)
        except FileNotFoundError as error:
            raise BevyFeedbackError(
                f"protocol file not found at {self.protocol_file}; is the game running under 'bevy-feedback run'?"
            ) from error
        version = protocol.get("protocol")
        if version != PROTOCOL_VERSION:
            raise BevyFeedbackError(
                f"protocol_version_mismatch: game uses {version!r}, client expects {PROTOCOL_VERSION}; "
                "upgrade or downgrade the client and game to the same 0.5 release"
            )
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


def _protocol_positive_int(
    protocol: dict[str, Any], key: str, default: int, required: bool
) -> int:
    if key not in protocol:
        if required:
            raise BevyFeedbackError(f"protocol missing required advertised cap {key!r}")
        return default
    return _positive_int(key, protocol[key])


def _protocol_positive_float(
    protocol: dict[str, Any], key: str, default: float, required: bool
) -> float:
    if key not in protocol:
        if required:
            raise BevyFeedbackError(f"protocol missing required advertised cap {key!r}")
        return default
    return _positive_seconds(key, protocol[key])


def _protocol_bool(
    protocol: dict[str, Any], key: str, default: bool, required: bool
) -> bool:
    if key not in protocol:
        if required:
            raise BevyFeedbackError(f"protocol missing required advertised field {key!r}")
        return default
    value = protocol[key]
    if not isinstance(value, bool):
        raise BevyFeedbackError(f"protocol field {key!r} must be a boolean")
    return value


def _positive_int(name: str, value: Any) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise BevyFeedbackError(f"{name} must be a positive integer")
    return value


def _bounded_frames(name: str, value: Any, cap: int) -> int:
    value = _positive_int(name, value)
    if value > cap:
        raise BevyFeedbackError(f"{name} must not exceed advertised max_wait_frames {cap}")
    return value


def _wait_limit(name: str, requested: int, supported: int) -> None:
    if requested > supported:
        raise BevyFeedbackError(
            f"{name}={requested} exceeds server limit {supported}; "
            "configure AgentFeedbackConfig.max_wait_frames or issue explicit bounded requests"
        )


def _bounded_abort_predicates(
    predicates: Sequence[dict[str, Any]], supported: int
) -> tuple[dict[str, Any], ...]:
    if not isinstance(predicates, Sequence) or isinstance(predicates, (str, bytes)):
        raise BevyFeedbackError("abort_predicates must be a sequence of predicate mappings")
    requested = len(predicates)
    if requested > supported:
        raise BevyFeedbackError(
            f"abort_predicates has {requested} items, but server supports {supported}; "
            "reduce abort predicates or configure separate explicit waits"
        )
    if any(not isinstance(predicate, dict) for predicate in predicates):
        raise BevyFeedbackError("abort_predicates entries must be predicate mappings")
    return tuple(predicates)


def _positive_seconds(name: str, value: Any) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise BevyFeedbackError(f"{name} must be a finite positive number")
    value = float(value)
    if not math.isfinite(value) or value <= 0.0:
        raise BevyFeedbackError(f"{name} must be a finite positive number")
    return value


def _duration_nanoseconds(name: str, value: Any) -> int:
    value = _positive_seconds(name, value)
    try:
        nanoseconds = int(
            (Decimal(str(value)) * NANOSECONDS_PER_SECOND).to_integral_value(
                rounding=ROUND_HALF_EVEN
            )
        )
    except (InvalidOperation, ValueError) as error:
        raise BevyFeedbackError(f"{name} cannot be converted to nanoseconds") from error
    if nanoseconds <= 0:
        raise BevyFeedbackError(f"{name} must be at least one nanosecond")
    return nanoseconds


def _selector(target: dict[str, str]) -> dict[str, str]:
    if not isinstance(target, dict):
        raise BevyFeedbackError("target must be a selector mapping")
    keys = ("name", "accessibility_label", "marker")
    selected = [key for key in keys if key in target]
    if len(selected) != 1 or len(target) != 1:
        raise BevyFeedbackError(
            "target must contain exactly one of name, accessibility_label, or marker"
        )
    value = target[selected[0]]
    if not isinstance(value, str) or not value:
        raise BevyFeedbackError("target selector value must be a nonempty string")
    return {selected[0]: value}


def _target_request(
    command: str,
    target: dict[str, str],
    kind: str | None,
    camera: str | None,
) -> dict[str, Any]:
    if kind is not None and kind not in ("any", "ui", "world"):
        raise BevyFeedbackError("kind must be 'any', 'ui', or 'world'")
    request: dict[str, Any] = {
        "command": command,
        "target": _selector(target),
    }
    if kind is not None:
        request["kind"] = kind
    if camera is not None:
        request["camera"] = camera
    return request


def _target_predicate(
    predicate_type: str,
    target: dict[str, str],
    kind: str | None,
    camera: str | None,
) -> dict[str, Any]:
    predicate = _target_request(predicate_type, target, kind, camera)
    predicate["type"] = predicate.pop("command")
    return predicate


def _marker_predicate(
    marker: str, min_count: int | None, max_count: int | None
) -> dict[str, Any]:
    if min_count is None and max_count is None:
        raise BevyFeedbackError("marker count requires min_count or max_count")
    predicate: dict[str, Any] = {"type": "marker_count", "marker": marker}
    if min_count is not None:
        predicate["min"] = _nonnegative_int("min_count", min_count)
    if max_count is not None:
        predicate["max"] = _nonnegative_int("max_count", max_count)
    if min_count is not None and max_count is not None and min_count > max_count:
        raise BevyFeedbackError("min_count must not exceed max_count")
    return predicate


def _nonnegative_int(name: str, value: Any) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise BevyFeedbackError(f"{name} must be a nonnegative integer")
    return value


def _bounded_attempts(value: Any) -> int:
    value = _positive_int("attempts", value)
    if value > MAX_CLIENT_CHUNKS:
        raise BevyFeedbackError(
            f"attempts must not exceed client bound {MAX_CLIENT_CHUNKS}"
        )
    return value


def _response_details(response: dict[str, Any]) -> dict[str, Any]:
    result = response.get("result")
    details = result.get("details") if isinstance(result, dict) else None
    if not isinstance(details, dict):
        raise BevyFeedbackError(f"response missing diagnostic details: {response}")
    return details


def pixel_diff(
    a: str | os.PathLike[str],
    b: str | os.PathLike[str],
    region: tuple[int, int, int, int] | None = None,
    *,
    include: tuple[int, int, int, int] | None = None,
    masks: Sequence[tuple[int, int, int, int]] = (),
) -> int:
    """Count differing physical PNG pixels, optionally inside an include and outside masks."""
    if region is not None and include is not None:
        raise BevyFeedbackError("pixel_diff accepts either region or include, not both")
    include = include if include is not None else region
    masks = _bounded_masks(masks)
    try:
        from PIL import Image
    except ImportError as error:
        raise BevyFeedbackError("Pillow is required for image assertions") from error
    image_a = Image.open(a).convert("RGBA")
    image_b = Image.open(b).convert("RGBA")
    scan_a, masks_a = _validate_pixel_regions(image_a.width, image_a.height, include, masks)
    _validate_pixel_regions(image_b.width, image_b.height, include, masks)
    if image_a.size != image_b.size:
        raise BevyFeedbackError(
            f"image dimensions differ: {image_a.size} vs {image_b.size} "
            "(window resized; wait_until_stable + re-capture)"
        )
    x, y, width, height = scan_a
    changed = 0
    for py in range(y, y + height):
        for px in range(x, x + width):
            masked = False
            for mx, my, mask_width, mask_height in masks_a:
                if mx <= px < mx + mask_width and my <= py < my + mask_height:
                    masked = True
                    break
            if not masked:
                changed += image_a.getpixel((px, py)) != image_b.getpixel((px, py))
    return changed


def region_diff(
    a: str | os.PathLike[str],
    b: str | os.PathLike[str],
    region: tuple[int, int, int, int],
    *,
    masks: Sequence[tuple[int, int, int, int]] = (),
) -> int:
    """Count differing physical PNG pixels inside a region and outside masks."""
    return pixel_diff(a, b, include=region, masks=masks)


def _bounded_masks(
    masks: Sequence[tuple[int, int, int, int]],
) -> tuple[tuple[int, int, int, int], ...]:
    if not isinstance(masks, Sequence) or isinstance(masks, (str, bytes)):
        raise BevyFeedbackError("masks must be a sequence of rectangle tuples")
    count = len(masks)
    if count > 8:
        raise BevyFeedbackError(f"at most 8 mask rectangles are allowed, got {count}")
    return tuple(masks[index] for index in range(count))


def _validate_pixel_regions(
    image_width: int,
    image_height: int,
    include: tuple[int, int, int, int] | None,
    masks: tuple[tuple[int, int, int, int], ...],
) -> tuple[tuple[int, int, int, int], tuple[tuple[int, int, int, int], ...]]:
    scan = _validate_pixel_region(
        include if include is not None else (0, 0, image_width, image_height),
        image_width,
        image_height,
        "include",
    )
    validated_masks = tuple(
        _validate_pixel_region(mask, image_width, image_height, f"mask[{index}]")
        for index, mask in enumerate(masks)
    )
    return scan, validated_masks


def _validate_pixel_region(
    region: tuple[int, int, int, int],
    image_width: int,
    image_height: int,
    name: str,
) -> tuple[int, int, int, int]:
    if not isinstance(region, tuple) or len(region) != 4:
        raise BevyFeedbackError(f"{name} must be a 4-integer tuple, got {region!r}")
    if any(isinstance(value, bool) or not isinstance(value, int) for value in region):
        raise BevyFeedbackError(f"{name} must be a 4-integer tuple, got {region!r}")
    x, y, width, height = region
    if x < 0 or y < 0:
        raise BevyFeedbackError(f"{name} origin must be nonnegative, got {region!r}")
    if width <= 0 or height <= 0:
        raise BevyFeedbackError(f"{name} dimensions must be nonzero, got {region!r}")
    if x > image_width or width > image_width - x or y > image_height or height > image_height - y:
        raise BevyFeedbackError(
            f"{name} {region!r} is out of bounds for image {image_width}x{image_height}"
        )
    return region


def image_has_color(
    path: str | os.PathLike[str],
    color: tuple[int, int, int],
    region: tuple[int, int, int, int] | None = None,
    tolerance: int = 0,
) -> bool:
    return color_pixel_count(path, color, region, tolerance) > 0


def color_pixel_count(
    path: str | os.PathLike[str],
    color: tuple[int, int, int],
    region: tuple[int, int, int, int] | None = None,
    tolerance: int = 0,
) -> int:
    try:
        from PIL import Image
    except ImportError as error:
        raise BevyFeedbackError("Pillow is required for image assertions") from error
    image = Image.open(path).convert("RGB")
    x, y, width, height = region or (0, 0, image.width, image.height)
    found = 0
    for py in range(y, y + height):
        for px in range(x, x + width):
            pixel = image.getpixel((px, py))
            if all(abs(pixel[index] - color[index]) <= tolerance for index in range(3)):
                found += 1
    return found


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


def fail(message: str) -> NoReturn:
    raise BevyFeedbackError(message)


def run(driver: Callable[[BevyFeedbackClient], None]) -> NoReturn:
    try:
        with BevyFeedbackClient() as client:
            driver(client)
    except (BevyFeedbackError, OSError, KeyboardInterrupt) as error:
        print(json.dumps({"ok": False, "error": str(error)}, separators=(",", ":")))
        raise SystemExit(1)
    print(json.dumps({"ok": True}, separators=(",", ":")))
    raise SystemExit(0)


def drive_stdio(protocol_file: str | os.PathLike[str], lines: Iterable[str]) -> int:
    """Compatibility helper: stdin JSON-lines to stdout JSON-lines."""
    failed = False
    with BevyFeedbackClient(protocol_file) as client:
        for line_number, line in enumerate(lines, 1):
            line = line.strip()
            if not line:
                continue
            try:
                command = json.loads(line)
            except json.JSONDecodeError as error:
                failed = True
                print(
                    json.dumps(
                        {
                            "id": None,
                            "ok": False,
                            "error": {
                                "code": "invalid_json",
                                "message": f"line {line_number}: {error}",
                            },
                        },
                        separators=(",", ":"),
                    ),
                    flush=True,
                )
                continue
            command.setdefault("id", line_number)
            try:
                response = client.request(command)
            except BevyFeedbackError as error:
                failed = True
                response = {
                    "id": command.get("id", line_number),
                    "ok": False,
                    "error": {"code": "client_error", "message": str(error)},
                }
            print(json.dumps(response, separators=(",", ":")), flush=True)
    return 1 if failed else 0
