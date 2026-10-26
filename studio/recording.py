"""Bounded IQ pre-roll, SigMF cu8 recording/replay, and independent mono WAV.

IQ datasets contain unsigned interleaved I,Q bytes without headers. Metadata is
SigMF 1.2.6 (https://sigmf.org/): core:datatype=cu8, captures carry original
stream sample indices, and gaps never become invented zero-valued samples.
The optional ``spectrum_studio`` 0.1.0 namespace records input settings, source
indices, stop state, trigger marks and discontinuities. Its dictionaries are
ordinary JSON and are intentionally retained when replaying.

The writer does not acquire a receiver or change any instrument settings.
"""
from __future__ import annotations

from collections import deque
from datetime import datetime, timedelta, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import re
import threading
from typing import Iterable
import uuid
import wave

import numpy as np


def _positive(value, name):
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise ValueError(f"{name} must be positive and finite")
    return number


def _seconds(value, name="seconds"):
    number = float(value)
    if not math.isfinite(number) or number < 0:
        raise ValueError(f"{name} must be nonnegative and finite")
    return number


def _sample_index(value):
    result = int(value)
    if result < 0 or result != value:
        raise ValueError("sample index must be a nonnegative integer")
    return result


def _raw_bytes(raw):
    data = bytes(raw)
    if len(data) % 2:
        raise ValueError("IQ data must contain complete interleaved I/Q pairs")
    return data


def _utc_now():
    return datetime.now(timezone.utc)


def _utc_text(value):
    return value.astimezone(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")


def _json_safe(value):
    if isinstance(value, dict):
        return {str(k): _json_safe(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_json_safe(v) for v in value]
    if isinstance(value, np.generic):
        return value.item()
    if isinstance(value, Path):
        return str(value)
    return value


def _atomic_json(path, value, first=False):
    text = json.dumps(_json_safe(value), ensure_ascii=False, allow_nan=False, indent=2) + "\n"
    if first:
        with path.open("x", encoding="utf-8") as handle:
            handle.write(text)
        return
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex + ".tmp")
    with temporary.open("x", encoding="utf-8") as handle:
        handle.write(text)
        handle.flush()
    os.replace(temporary, path)


def _new_stem(prefix):
    safe = re.sub(r"[^\w-]", "_", str(prefix), flags=re.UNICODE)[:48] or "capture"
    return f"{safe}_{_utc_now():%Y%m%dT%H%M%S_%fZ}_{uuid.uuid4().hex[:8]}"


class PreTriggerBuffer:
    """Keep only bounded, contiguous recent cu8 samples.

    ``get(seconds, end_sample=None)`` returns ``(chunks, available_seconds)``.
    With an explicit end, samples at or after that stream index are excluded.
    A detected discontinuity starts a fresh pre-roll instead of hiding a gap.
    """

    def __init__(self, sample_rate, max_bytes=128 * 1024 * 1024):
        self.sample_rate = _positive(sample_rate, "sample_rate")
        self.max_bytes = int(max_bytes) // 2 * 2
        if self.max_bytes < 2:
            raise ValueError("pre-trigger capacity must hold at least one I/Q pair")
        self._chunks = deque()
        self._bytes = 0
        self._lock = threading.RLock()
        self.gap_count = 0

    @property
    def capacity_seconds(self):
        return self.max_bytes / 2 / self.sample_rate

    @property
    def available_seconds(self):
        with self._lock:
            return self._bytes / 2 / self.sample_rate

    @property
    def bytes_used(self):
        with self._lock:
            return self._bytes

    def clear(self):
        with self._lock:
            self._chunks.clear()
            self._bytes = 0
            self.gap_count = 0

    def append(self, start_sample, raw):
        start = _sample_index(start_sample)
        data = _raw_bytes(raw)
        if not data:
            return
        with self._lock:
            if self._chunks:
                previous_start, previous_data = self._chunks[-1]
                expected = previous_start + len(previous_data) // 2
                if start < expected:
                    covered = min(len(data) // 2, expected - start)
                    data = data[covered * 2:]
                    start += covered
                    if not data:
                        return
                elif start > expected:
                    self._chunks.clear()
                    self._bytes = 0
                    self.gap_count += 1
            if len(data) > self.max_bytes:
                skipped = (len(data) - self.max_bytes) // 2
                data = data[skipped * 2:]
                start += skipped
            self._chunks.append((start, data))
            self._bytes += len(data)
            while self._bytes > self.max_bytes:
                head_start, head_data = self._chunks.popleft()
                excess = self._bytes - self.max_bytes
                if len(head_data) <= excess:
                    self._bytes -= len(head_data)
                else:
                    self._chunks.appendleft((head_start + excess // 2, head_data[excess:]))
                    self._bytes -= excess

    def get(self, seconds, end_sample=None):
        count = int(_seconds(seconds) * self.sample_rate)
        with self._lock:
            if not self._chunks or count == 0:
                return [], 0.0
            newest_start, newest_data = self._chunks[-1]
            end = newest_start + len(newest_data) // 2
            if end_sample is not None:
                end = min(end, _sample_index(end_sample))
            first = max(self._chunks[0][0], end - count)
            chunks = []
            for start, data in self._chunks:
                stop = start + len(data) // 2
                low, high = max(first, start), min(end, stop)
                if high > low:
                    chunks.append((low, data[(low-start)*2:(high-start)*2]))
            available = sum(len(data) for _, data in chunks) / 2 / self.sample_rate
            return chunks, available


class IQRecorder:
    """Write bounded SigMF cu8 data, retaining partial files on I/O failure.

    ``write`` returns the count actually accepted. On an I/O failure or a size /
    source-duration cap the writer safely closes itself. The engine should then
    report the reason and Disarm the corresponding IQ trigger.
    """

    def __init__(self, output_dir, sample_rate, center_hz, input_metadata=None,
                 max_bytes=512 * 1024 * 1024, max_seconds=120):
        self.output_dir = Path(output_dir).expanduser().resolve()
        self.sample_rate = _positive(sample_rate, "sample_rate")
        self.center_hz = float(center_hz)
        if not math.isfinite(self.center_hz):
            raise ValueError("center_hz must be finite")
        self.input_metadata = _json_safe(dict(input_metadata or {}))
        json.dumps(self.input_metadata, allow_nan=False)
        self.max_bytes = int(max_bytes) // 2 * 2
        self.max_seconds = _positive(max_seconds, "max_seconds")
        if self.max_bytes < 2 or self.max_seconds * self.sample_rate < 1:
            raise ValueError("recording capacity must hold at least one sample")
        self._lock = threading.RLock()
        self.active = False
        self.limit_reached = False
        self.last_error = None
        self.stop_reason = None
        self.data_path = self.meta_path = self.path = None
        self.samples_written = self.bytes_written = 0
        self._file = None
        self._next_source = self._origin = None
        self._captures = []
        self._annotations = []
        self._gaps = []
        self._hasher = hashlib.sha512()
        self._started_at = None
        self._last_result = None

    @property
    def duration_seconds(self):
        return self.samples_written / self.sample_rate

    def _metadata(self):
        return {
            "global": {
                "core:datatype": "cu8", "core:version": "1.2.6",
                "core:sample_rate": self.sample_rate,
                "core:recorder": "RTL-SDR Spectrum Studio",
                "core:description": "Unsigned interleaved I/Q; offsets are original source sample indices.",
                "core:sha512": self._hasher.hexdigest(),
                "core:extensions": [{"name": "spectrum_studio", "version": "0.1.0", "optional": True}],
                "spectrum_studio:input": self.input_metadata,
                "spectrum_studio:recording": {
                    "state": "recording" if self.active else ("incomplete" if self.last_error or self.limit_reached else "complete"),
                    "samples": self.samples_written, "bytes": self.bytes_written,
                    "started_at_utc": _utc_text(self._started_at),
                    "source_sample_start": self._origin,
                    "source_sample_stop": self._next_source,
                    "stop_reason": self.stop_reason, "limit_reached": self.limit_reached,
                    "error": self.last_error, "gaps": self._gaps,
                    "max_bytes": self.max_bytes, "max_seconds": self.max_seconds,
                    "iq_normalization": "(unsigned_byte - 127.5) / 127.5",
                },
            },
            "captures": self._captures,
            "annotations": sorted(self._annotations, key=lambda item: item["core:sample_start"]),
        }

    def _summary(self):
        return {"data_path": str(self.data_path) if self.data_path else None,
                "meta_path": str(self.meta_path) if self.meta_path else None,
                "path": str(self.meta_path) if self.meta_path else None,
                "samples": self.samples_written, "bytes": self.bytes_written,
                "duration_seconds": self.duration_seconds, "stop_reason": self.stop_reason,
                "limit_reached": self.limit_reached, "error": self.last_error,
                "active": self.active}

    def start(self, pre_chunks=None):
        with self._lock:
            if self.active:
                raise RuntimeError("IQ recording is already active")
            self.output_dir.mkdir(parents=True, exist_ok=True)
            stem = _new_stem("iq")
            self.data_path = self.output_dir / (stem + ".sigmf-data")
            self.meta_path = self.output_dir / (stem + ".sigmf-meta")
            self.path = self.meta_path
            self.samples_written = self.bytes_written = 0
            self._next_source = self._origin = None
            self._captures, self._annotations, self._gaps = [], [], []
            self._hasher = hashlib.sha512()
            self.limit_reached = False
            self.last_error = self.stop_reason = None
            self._started_at = _utc_now()
            self._file = self.data_path.open("xb")
            self.active = True
            try:
                _atomic_json(self.meta_path, self._metadata(), first=True)
                for source_start, raw in pre_chunks or []:
                    self.write(raw, source_start)
                    if not self.active:
                        break
            except Exception as exc:
                self.last_error = f"{type(exc).__name__}: {exc}"
                self.stop("start_error")
                raise
            return self._summary()

    def _capture(self, source_start):
        stamp = self._started_at
        supplied = self.input_metadata.get("stream_start_utc")
        if supplied:
            try:
                base = datetime.fromisoformat(str(supplied).replace("Z", "+00:00"))
                if base.tzinfo is None:
                    raise ValueError("stream_start_utc must include timezone")
                stamp = base + timedelta(seconds=source_start/self.sample_rate)
            except (ValueError, TypeError, OverflowError):
                supplied = None
        self._captures.append({"core:sample_start": self.samples_written,
                               "core:global_index": source_start,
                               "core:frequency": self.center_hz,
                               "core:datetime": _utc_text(stamp),
                               "spectrum_studio:timestamp_source": "source_host_clock" if supplied else "recording_host_estimate"})

    def write(self, raw_u8bytes, start_sample):
        data = _raw_bytes(raw_u8bytes)
        start = _sample_index(start_sample)
        with self._lock:
            if not self.active or not data:
                return 0
            if self._next_source is not None and start < self._next_source:
                covered = min(len(data)//2, self._next_source-start)
                data, start = data[covered*2:], start+covered
                if not data:
                    return 0
            if self._origin is None:
                self._origin = start
                self._capture(start)
            elif start > self._next_source:
                self._gaps.append({"source_sample_start": self._next_source,
                                   "missing_samples": start-self._next_source,
                                   "file_sample_start": self.samples_written})
                self._capture(start)
            byte_remaining = (self.max_bytes-self.bytes_written)//2
            time_remaining = self._origin + int(self.max_seconds*self.sample_rate) - start
            accepted = min(len(data)//2, byte_remaining, max(0, time_remaining))
            clipped = data[:accepted*2]
            written = 0
            try:
                if clipped:
                    written_bytes = self._file.write(clipped)
                    if written_bytes != len(clipped):
                        raise OSError("short IQ file write")
                    self._hasher.update(clipped)
                    self.bytes_written += written_bytes
                    written = written_bytes//2
                    self.samples_written += written
                    self._next_source = start+written
                at_bytes = self.bytes_written >= self.max_bytes
                at_time = time_remaining <= accepted
                if accepted < len(data)//2 or at_bytes or at_time:
                    self.limit_reached = True
                    self.stop("size_limit" if at_bytes or byte_remaining <= max(0, time_remaining) else "duration_limit")
            except OSError as exc:
                self.last_error = f"{type(exc).__name__}: {exc}"
                # Preserve only complete bytes acknowledged before this failed write.
                try:
                    self._file.truncate(self.bytes_written)
                except OSError:
                    pass
                self.stop("io_error")
            return written

    def mark_trigger(self, sample_index, details=None):
        source_sample = _sample_index(sample_index)
        with self._lock:
            file_sample = self.samples_written
            for capture in reversed(self._captures):
                if source_sample >= capture["core:global_index"]:
                    file_sample = capture["core:sample_start"] + source_sample-capture["core:global_index"]
                    break
            self._annotations.append({"core:sample_start": max(0, min(file_sample, self.samples_written)),
                                      "core:sample_count": 0,
                                      "core:label": "trigger",
                                      "spectrum_studio:source_sample": source_sample,
                                      "spectrum_studio:trigger": _json_safe(details or {})})

    add_trigger = mark_trigger

    def checkpoint(self):
        with self._lock:
            if self.meta_path:
                _atomic_json(self.meta_path, self._metadata())

    def stop(self, reason="manual"):
        with self._lock:
            if not self.active:
                return self._summary()
            self.active = False
            self.stop_reason = str(reason)
            try:
                self._file.flush()
                os.fsync(self._file.fileno())
            except (OSError, ValueError) as exc:
                self.last_error = self.last_error or f"{type(exc).__name__}: {exc}"
                self.stop_reason = "io_error"
            finally:
                try:
                    self._file.close()
                except OSError as exc:
                    self.last_error = self.last_error or str(exc)
                self._file = None
            try:
                _atomic_json(self.meta_path, self._metadata())
            except (OSError, ValueError, TypeError) as exc:
                self.last_error = self.last_error or f"metadata: {exc}"
            self._last_result = self._summary()
            return dict(self._last_result)


class ReplayReader:
    """Seek/read cu8 SigMF. Seek time refers to stored samples, not gap padding."""

    def __init__(self, path):
        self.meta_path = Path(path).expanduser().resolve()
        if self.meta_path.suffix == ".sigmf-data":
            self.meta_path = self.meta_path.with_suffix(".sigmf-meta")
        self.metadata = json.loads(self.meta_path.read_text(encoding="utf-8-sig"))
        global_meta = self.metadata["global"]
        if global_meta.get("core:datatype") != "cu8":
            raise ValueError("this version replays SigMF cu8 recordings only")
        if global_meta.get("core:metadata_only", False):
            raise ValueError("metadata-only SigMF has no samples to replay")
        if global_meta.get("core:num_channels", 1) != 1:
            raise ValueError("this version replays one complex IQ channel only")
        for extension in global_meta.get("core:extensions", []):
            supported = extension.get("name") == "spectrum_studio" and extension.get("version") == "0.1.0"
            if not extension.get("optional", False) and not supported:
                raise ValueError("unsupported required SigMF extension: " + str(extension.get("name")))
        self.sample_rate = _positive(global_meta["core:sample_rate"], "sample_rate")
        self.data_path = self.meta_path.with_suffix(".sigmf-data")
        if "core:dataset" in global_meta:
            name = str(global_meta["core:dataset"])
            if Path(name).name != name:
                raise ValueError("dataset must be a file in the metadata directory")
            self.data_path = self.meta_path.parent / name
        length = self.data_path.stat().st_size
        if length % 2:
            raise ValueError("truncated I/Q sample pair")
        self.total_samples = length//2
        self.captures = sorted(self.metadata.get("captures", []), key=lambda item: item["core:sample_start"])
        if not self.captures:
            self.captures = [{"core:sample_start": 0, "core:global_index": 0}]
        if self.captures[0]["core:sample_start"] != 0:
            raise ValueError("first capture must start at sample zero")
        self.center_hz = float(self.captures[0].get("core:frequency", 0))
        if not math.isfinite(self.center_hz):
            raise ValueError("capture center frequency must be finite")
        if global_meta.get("core:trailing_bytes", 0) or any(c.get("core:header_bytes", 0) for c in self.captures):
            raise ValueError("non-conforming SigMF datasets with embedded headers/trailers are not supported")
        if any(float(c.get("core:frequency", self.center_hz)) != self.center_hz for c in self.captures):
            raise ValueError("frequency-changing recordings require a segment-aware replay mode")
        self.input_metadata = global_meta.get("spectrum_studio:input", {})
        self.recording_metadata = global_meta.get("spectrum_studio:recording", {})
        self.warnings = []
        if self.recording_metadata.get("state") != "complete":
            self.warnings.append("recording was not finalized normally")
        if self.recording_metadata.get("gaps"):
            self.warnings.append("source discontinuities are described in metadata")
        self._file = self.data_path.open("rb")
        self.position_samples = 0

    @property
    def duration_seconds(self):
        return self.total_samples/self.sample_rate

    @property
    def position_seconds(self):
        return self.position_samples/self.sample_rate

    def seek_seconds(self, seconds):
        return self.seek_samples(int(_seconds(seconds)*self.sample_rate))

    def seek_samples(self, sample_index):
        self.position_samples = min(self.total_samples, _sample_index(sample_index))
        self._file.seek(self.position_samples*2)
        return self.position_samples

    def read_chunk(self, samples=131072):
        count = int(samples)
        if count <= 0:
            raise ValueError("chunk size must be positive")
        start = self.position_samples
        data = self._file.read(count*2)
        if len(data) % 2:
            raise ValueError("truncated I/Q sample pair while reading")
        self.position_samples += len(data)//2
        return start, data

    def iter_chunks(self, chunk_samples=131072):
        while True:
            start, data = self.read_chunk(chunk_samples)
            if not data:
                break
            yield start, data

    def global_sample_for(self, file_sample):
        sample = _sample_index(file_sample)
        capture = self.captures[0]
        for item in self.captures:
            if item["core:sample_start"] > sample:
                break
            capture = item
        return int(capture.get("core:global_index", capture["core:sample_start"])) + sample-capture["core:sample_start"]

    def __iter__(self):
        return self.iter_chunks()

    def close(self):
        self._file.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


class WavRecorder:
    """Per-VFO PCM16/PCM24 mono WAV; levels are independent of speaker gain."""

    FORMATS = {"PCM16": 2, "PCM24": 3}

    def __init__(self, output_dir, vfo_id, audio_rate=32000, format="PCM16",
                 condition_metadata=None, max_bytes=0xFFFFFFFF-36):
        self.output_dir = Path(output_dir).expanduser().resolve()
        self.vfo_id = str(vfo_id)
        self.audio_rate = int(_positive(audio_rate, "audio_rate"))
        self.format = str(format).upper()
        if self.format not in self.FORMATS:
            raise ValueError("WAV format must be PCM16 or PCM24")
        self.sample_width = self.FORMATS[self.format]
        self.max_bytes = min(int(max_bytes), 0xFFFFFFFF-36)
        if self.max_bytes < self.sample_width:
            raise ValueError("WAV capacity must hold at least one sample")
        self.condition_metadata = _json_safe(dict(condition_metadata or {}))
        self._lock = threading.RLock()
        self.active = False
        self.last_error = self.stop_reason = None
        self.path = self.meta_path = None
        self.samples_written = self.bytes_written = self.clipped_samples = 0
        self.limit_reached = False
        self.segment_index = 0
        self.segments = []
        self._wave = self._file = None
        self._started_at = None

    def _summary(self):
        return {"path": str(self.path) if self.path else None,
                "meta_path": str(self.meta_path) if self.meta_path else None,
                "vfo_id": self.vfo_id, "format": self.format, "audio_rate": self.audio_rate,
                "samples": self.samples_written, "bytes": self.bytes_written,
                "duration_seconds": self.samples_written/self.audio_rate,
                "clipped_samples": self.clipped_samples, "segment_index": self.segment_index,
                "conditions": self.condition_metadata, "active": self.active,
                "stop_reason": self.stop_reason, "error": self.last_error,
                "limit_reached": self.limit_reached,
                "started_at_utc": _utc_text(self._started_at) if self._started_at else None}

    def start(self, condition_metadata=None):
        with self._lock:
            if self.active:
                raise RuntimeError("WAV recording is already active")
            if condition_metadata is not None:
                self.condition_metadata = _json_safe(dict(condition_metadata))
            json.dumps(self.condition_metadata, allow_nan=False)
            self.output_dir.mkdir(parents=True, exist_ok=True)
            self.path = self.output_dir / (_new_stem("vfo_"+self.vfo_id) + ".wav")
            self.meta_path = self.path.with_suffix(".wav.json")
            self._started_at = _utc_now()
            self.samples_written = self.bytes_written = self.clipped_samples = 0
            self.last_error = self.stop_reason = None
            self.limit_reached = False
            self._file = self.path.open("xb")
            try:
                self._wave = wave.open(self._file, "wb")
                self._wave.setnchannels(1)
                self._wave.setsampwidth(self.sample_width)
                self._wave.setframerate(self.audio_rate)
                self.active = True
                _atomic_json(self.meta_path, self._summary(), first=True)
            except Exception:
                if self._wave:
                    self._wave.close()
                self._file.close()
                self.active = False
                raise
            return self._summary()

    def write(self, audio):
        values = np.asarray(audio, dtype=np.float64)
        if values.ndim != 1:
            raise ValueError("WAV audio must be a one-dimensional mono array")
        with self._lock:
            if not self.active or not len(values):
                return 0
            if not np.isfinite(values).all():
                self.last_error = "non-finite audio samples"
                self.stop("invalid_audio")
                return 0
            available = (self.max_bytes-self.bytes_written)//self.sample_width
            accepted = min(len(values), available)
            values = values[:accepted]
            limit = 1 << (8*self.sample_width-1)
            self.clipped_samples += int(np.count_nonzero((values < -1) | (values > (limit-1)/limit)))
            integer = np.rint(np.clip(values, -1, (limit-1)/limit)*limit).astype(np.int32)
            if self.sample_width == 2:
                data = integer.astype("<i2").tobytes()
            else:
                data = ((integer[:, None] >> np.array([0, 8, 16])) & 255).astype(np.uint8).tobytes()
            try:
                self._wave.writeframesraw(data)
                self.samples_written += accepted
                self.bytes_written += len(data)
                if accepted < len(audio) or self.bytes_written+self.sample_width > self.max_bytes:
                    self.limit_reached = True
                    self.stop("size_limit")
            except (OSError, wave.Error) as exc:
                self.last_error = f"{type(exc).__name__}: {exc}"
                self.stop("io_error")
                return 0
            return accepted

    def stop(self, reason="manual"):
        with self._lock:
            if not self.active:
                return self._summary()
            self.active = False
            self.stop_reason = str(reason)
            try:
                self._wave.close()
                self._file.flush()
                os.fsync(self._file.fileno())
            except (OSError, wave.Error, ValueError) as exc:
                self.last_error = self.last_error or f"{type(exc).__name__}: {exc}"
                self.stop_reason = "io_error"
            finally:
                self._file.close()
                self._wave = self._file = None
            summary = self._summary()
            try:
                _atomic_json(self.meta_path, summary)
            except (OSError, ValueError, TypeError) as exc:
                self.last_error = self.last_error or f"metadata: {exc}"
                summary = self._summary()
            self.segments.append(dict(summary))
            return summary

    def split(self, new_condition_metadata):
        with self._lock:
            was_active = self.active
            previous = self.stop("conditions_changed") if was_active else None
            self.condition_metadata = _json_safe(dict(new_condition_metadata or {}))
            self.segment_index += 1
            current = self.start() if was_active else self._summary()
            return {"previous": previous, "current": current}
