"""Bounded RTL-SDR asynchronous source and explicitly labelled synthetic source."""
from __future__ import annotations

import ctypes as C
from dataclasses import dataclass
import os
from pathlib import Path
import queue
import threading
import time

import numpy as np

PROJECT = Path(__file__).resolve().parents[1]
DEFAULT_DLL = PROJECT / "tools/driver-setup-20261026/rtl-sdr-blog-V1.4.0/x64/rtlsdr.dll"
BLOCK_SAMPLES = 131072


def decode_iq(raw: bytes) -> np.ndarray:
    if len(raw) % 2:
        raise ValueError("IQデータが不完全です。I/Qは2バイトで1サンプルです。")
    data = np.frombuffer(raw, dtype=np.uint8).astype(np.float32)
    data = (data - 127.5) / 127.5
    return (data[0::2] + 1j * data[1::2]).astype(np.complex64)


def encode_iq(iq: np.ndarray) -> bytes:
    out = np.empty(iq.size * 2, dtype=np.uint8)
    out[0::2] = np.clip(np.rint(iq.real * 127.5 + 127.5), 0, 255)
    out[1::2] = np.clip(np.rint(iq.imag * 127.5 + 127.5), 0, 255)
    return out.tobytes()


class DeviceError(RuntimeError):
    pass


class RtlLibrary:
    def __init__(self, path: str | Path | None = None):
        self.path = Path(path or os.environ.get("STUDIO_RTLSDR_DLL", DEFAULT_DLL)).resolve()
        if not self.path.is_file():
            raise DeviceError(f"RTL-SDRのDLLが見つかりません: {self.path}")
        self._directory = os.add_dll_directory(str(self.path.parent)) if os.name == "nt" else None
        self.dll = C.CDLL(str(self.path))
        d = self.dll
        self.callback_type = C.CFUNCTYPE(None, C.POINTER(C.c_ubyte), C.c_uint32, C.c_void_p)
        specs = {
            "rtlsdr_get_device_count": ([], C.c_uint32),
            "rtlsdr_get_device_name": ([C.c_uint32], C.c_char_p),
            "rtlsdr_open": ([C.POINTER(C.c_void_p), C.c_uint32], C.c_int),
            "rtlsdr_close": ([C.c_void_p], C.c_int),
            "rtlsdr_set_sample_rate": ([C.c_void_p, C.c_uint32], C.c_int),
            "rtlsdr_get_sample_rate": ([C.c_void_p], C.c_uint32),
            "rtlsdr_set_center_freq": ([C.c_void_p, C.c_uint32], C.c_int),
            "rtlsdr_get_center_freq": ([C.c_void_p], C.c_uint32),
            "rtlsdr_set_tuner_gain_mode": ([C.c_void_p, C.c_int], C.c_int),
            "rtlsdr_set_tuner_gain": ([C.c_void_p, C.c_int], C.c_int),
            "rtlsdr_get_tuner_gain": ([C.c_void_p], C.c_int),
            "rtlsdr_get_tuner_gains": ([C.c_void_p, C.POINTER(C.c_int)], C.c_int),
            "rtlsdr_set_freq_correction": ([C.c_void_p, C.c_int], C.c_int),
            "rtlsdr_set_direct_sampling": ([C.c_void_p, C.c_int], C.c_int),
            "rtlsdr_get_direct_sampling": ([C.c_void_p], C.c_int),
            "rtlsdr_set_agc_mode": ([C.c_void_p, C.c_int], C.c_int),
            "rtlsdr_set_bias_tee": ([C.c_void_p, C.c_int], C.c_int),
            "rtlsdr_read_eeprom": ([C.c_void_p, C.POINTER(C.c_ubyte), C.c_uint8, C.c_uint16], C.c_int),
            "rtlsdr_reset_buffer": ([C.c_void_p], C.c_int),
            "rtlsdr_read_async": ([C.c_void_p, self.callback_type, C.c_void_p, C.c_uint32, C.c_uint32], C.c_int),
            "rtlsdr_cancel_async": ([C.c_void_p], C.c_int),
        }
        for name, (args, restype) in specs.items():
            fn = getattr(d, name)
            fn.argtypes, fn.restype = args, restype

    def devices(self) -> list[dict]:
        return [{"index": i, "name": (self.dll.rtlsdr_get_device_name(i) or b"RTL-SDR").decode(errors="replace")}
                for i in range(self.dll.rtlsdr_get_device_count())]


class RtlSource:
    def __init__(self, library: RtlLibrary, config: dict):
        self.library, self.config = library, dict(config)
        self.dev = C.c_void_p()
        self.queue: queue.Queue = queue.Queue(maxsize=24)
        self.stop_event = threading.Event()
        self.thread: threading.Thread | None = None
        self.error: str | None = None
        self.sample_index = self.dropped_samples = 0
        self.metadata: dict = {}
        self._callback = None

    def _check(self, code: int, action: str, allowed=()):
        if code != 0 and code not in allowed:
            raise DeviceError(f"{action}に失敗しました（RTL-SDR {code}）。")

    def open(self):
        d = self.library.dll
        available = self.library.devices()
        index = int(self.config.get("device_index", 0))
        if not any(item["index"] == index for item in available):
            raise DeviceError("RTL-SDRが見つかりません。USB接続とInterface 0のドライバーを確認してください。")
        self._check(d.rtlsdr_open(C.byref(self.dev), index), "受信機の接続")
        try:
            eeprom = (C.c_ubyte * 16)()
            rc = d.rtlsdr_read_eeprom(self.dev, eeprom, 0, 16)
            if rc < 0 or bytes(eeprom[:2]) != b"\x28\x32":
                raise DeviceError("Bias-Tの強制設定を確認できないため、受信を開始できません。")
            if not eeprom[7] & 0x02:
                raise DeviceError("機器にBias-T強制ONが設定されています。この構成では開始できません。")
            self._check(d.rtlsdr_set_bias_tee(self.dev, 0), "Bias-T OFF要求")
            center, fs = int(self.config["center_hz"]), int(self.config["sample_rate"])
            mode = self.config.get("mode", "auto")
            direct = 2 if mode == "direct_q" or (mode == "auto" and center < 24_000_000) else 0
            self._check(d.rtlsdr_set_direct_sampling(self.dev, direct), "入力モード設定")
            self._check(d.rtlsdr_set_sample_rate(self.dev, fs), "サンプルレート設定")
            self._check(d.rtlsdr_set_freq_correction(self.dev, int(self.config.get("ppm", 0))), "PPM補正", allowed=(-2,))
            self._check(d.rtlsdr_set_center_freq(self.dev, center), "中心周波数設定")
            agc = bool(self.config.get("agc", False))
            self._check(d.rtlsdr_set_agc_mode(self.dev, int(agc)), "AGC設定")
            gains = []
            if not direct:
                self._check(d.rtlsdr_set_tuner_gain_mode(self.dev, 0 if agc else 1), "チューナーゲイン設定")
                count = d.rtlsdr_get_tuner_gains(self.dev, None)
                if 0 < count < 256:
                    values = (C.c_int * count)()
                    got = d.rtlsdr_get_tuner_gains(self.dev, values)
                    if got > 0:
                        gains = [x / 10 for x in values[:got]]
                if not agc and gains:
                    actual = min(gains, key=lambda value: abs(value - float(self.config.get("gain_db", 20.7))))
                    self._check(d.rtlsdr_set_tuner_gain(self.dev, round(actual * 10)), "ゲイン設定")
            self._check(d.rtlsdr_reset_buffer(self.dev), "受信バッファ初期化")
            self.metadata = {
                "name": available[index]["name"], "device_index": index,
                "sample_rate": d.rtlsdr_get_sample_rate(self.dev),
                "center_hz": d.rtlsdr_get_center_freq(self.dev),
                "gain_db_requested": self.config.get("gain_db", 20.7),
                "gain_db_actual": None if direct or agc else d.rtlsdr_get_tuner_gain(self.dev) / 10,
                "agc": agc, "ppm": self.config.get("ppm", 0),
                "mode_requested": mode, "mode_actual": "direct_q" if d.rtlsdr_get_direct_sampling(self.dev) else "tuner",
                "available_gains_db": gains, "bias_tee_off_requested": True,
                "bias_tee_voltage_measured": False, "eeprom_written": False,
            }
            @self.library.callback_type
            def callback(pointer, length, context):
                start = self.sample_index
                self.sample_index += length // 2
                if self.stop_event.is_set():
                    return
                raw = C.string_at(pointer, length)
                try:
                    self.queue.put_nowait((start, raw))
                except queue.Full:
                    self.dropped_samples += length // 2
            self._callback = callback
            self.thread = threading.Thread(target=self._pump, daemon=True, name="rtl-usb-reader")
            self.thread.start()
            return dict(self.metadata)
        except Exception:
            d.rtlsdr_close(self.dev)
            self.dev = C.c_void_p()
            raise

    def _pump(self):
        rc = self.library.dll.rtlsdr_read_async(self.dev, self._callback, None, 12, BLOCK_SAMPLES * 2)
        if not self.stop_event.is_set():
            self.error = f"USB受信が停止しました（RTL-SDR {rc}）。"

    def read(self):
        try:
            return self.queue.get(timeout=0.5)
        except queue.Empty:
            if self.error:
                raise DeviceError(self.error)
            if self.thread and not self.thread.is_alive() and not self.stop_event.is_set():
                raise DeviceError("USB受信スレッドが終了しました。")
            return None

    def close(self):
        self.stop_event.set()
        if self.dev:
            self.library.dll.rtlsdr_cancel_async(self.dev)
        if self.thread:
            self.thread.join(timeout=4)
            if self.thread.is_alive():
                raise DeviceError("USB停止処理を待っています。設定を変更せず、機器状態を確認してください。")
        if self.dev:
            self.library.dll.rtlsdr_close(self.dev)
            self.dev = C.c_void_p()


class DemoSource:
    def __init__(self, config: dict):
        self.config = dict(config)
        self.sample_index = 0
        self.rng = np.random.default_rng(731)
        self.stop_event = threading.Event()
        self.deadline = time.monotonic()
        self.metadata = {**config, "name": "模擬IQ（実機ではありません）", "synthetic": True,
                         "gain_db_actual": None, "available_gains_db": []}

    def open(self):
        return dict(self.metadata)

    def read(self):
        fs = self.config["sample_rate"]
        self.deadline += BLOCK_SAMPLES / fs
        self.stop_event.wait(max(0, self.deadline - time.monotonic()))
        if self.stop_event.is_set():
            return None
        start = self.sample_index
        t = (np.arange(BLOCK_SAMPLES) + start) / fs
        target = float(self.config.get("demo_target_hz", self.config["center_hz"] - 256_000))
        offset = target - self.config["center_hz"]
        fm = .15 * np.exp(1j * (2*np.pi*offset*t + 75*np.sin(2*np.pi*1000*t)))
        am = .05 * (1 + .5*np.cos(2*np.pi*700*t)) * np.exp(2j*np.pi*350_000*t)
        noise = .005 * (self.rng.normal(size=BLOCK_SAMPLES) + 1j*self.rng.normal(size=BLOCK_SAMPLES))
        self.sample_index += BLOCK_SAMPLES
        return start, encode_iq(fm + am + noise)

    def close(self):
        self.stop_event.set()
