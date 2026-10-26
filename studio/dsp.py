"""Streaming, NumPy-only spectrum analysis and AM/NFM/WFM demodulation.

Power reference: complex input magnitude one has power one (0 dBFS).
PSD is two-sided dBFS/Hz. Spectrum/markers show PSD times window ENBW;
channel power always integrates the original linear PSD, not drawn traces.
"""
from __future__ import annotations

import base64
from collections import deque
from dataclasses import dataclass, replace
import math
import threading

import numpy as np


def power_db(value):
    """Finite power dB values suitable for JSON as well as array operations."""
    return 10.0 * np.log10(np.maximum(value, 1e-30))


def demod_audio_scale(mode):
    """Fixed PCM headroom, not AGC or a noise/SNR improvement.

    WFM nominal ±75 kHz maps to ±0.5 before audio filtering/de-emphasis.
    AM retains carrier-normalized envelope units; NFM retains nominal
    min(5 kHz, bandwidth/5) deviation units. The PCM sink owns clipping.
    """
    return 0.5 if str(mode).upper() == "WFM" else 1.0


def window_values(name: str, size: int):
    name = str(name).lower()
    if name in ("rect", "rectangular", "boxcar", "none"):
        return np.ones(size)
    if name in ("hann", "hanning"):
        return np.hanning(size + 1)[:-1]
    if name == "hamming":
        return np.hamming(size + 1)[:-1]
    if name == "blackman":
        return np.blackman(size + 1)[:-1]
    raise ValueError("Unsupported FFT window")


def fft_size_for_rbw(sample_rate, target_rbw_hz, window="hann"):
    if not math.isfinite(sample_rate) or sample_rate <= 0:
        raise ValueError("Sample rate must be positive")
    if not math.isfinite(target_rbw_hz) or target_rbw_hz <= 0:
        raise ValueError("RBW must be positive")
    candidates = [2**n for n in range(7, 16)]
    def error(size):
        values = window_values(window, size)
        enbw = sample_rate * np.sum(values**2) / np.sum(values)**2
        return abs(math.log(enbw / target_rbw_hz))
    return min(candidates, key=error)


@dataclass
class AnalysisConfig:
    sample_rate: float = 2_048_000
    fft_size: int = 4096
    window: str = "hann"
    overlap: float = .5
    ref_level: float = -20
    range_db: float = 100
    persistence_mode: str = "exponential"
    persistence_seconds: float = 1.0
    persistence_frames: int = 1000
    persistence_unit: str = "half_life"
    switch_policy: str = "convert"
    stats_mode: str = "persistence"
    density_metric: str = "rate"
    detector: str = "positive_peak"
    average_seconds: float = 1.0
    waterfall_row_seconds: float = .1
    density_min_db: float = -140
    density_max_db: float = 0
    density_levels: int = 128
    max_density_history_mb: float = 64
    waterfall_rows: int = 250

    def __post_init__(self):
        if not math.isfinite(self.sample_rate) or self.sample_rate <= 0:
            raise ValueError("Sample rate must be positive")
        if not 128 <= self.fft_size <= 32768 or self.fft_size & (self.fft_size-1):
            raise ValueError("FFT size must be a power of two in 128..32768")
        if not 0 <= self.overlap < .95:
            raise ValueError("FFT overlap must be in [0, .95)")
        window_values(self.window, self.fft_size)
        if self.persistence_mode.lower() not in ("off", "infinite", "exponential", "window", "time_window"):
            raise ValueError("Unknown Persistence mode")
        if self.persistence_unit not in ("half_life", "time_constant", "frames"):
            raise ValueError("Unknown Persistence unit")
        if self.switch_policy not in ("convert", "retain", "clear"):
            raise ValueError("Unknown Persistence switching policy")
        if self.stats_mode not in ("cumulative", "persistence"):
            raise ValueError("Unknown density statistics mode")
        if self.density_metric not in ("rate", "hits", "count"):
            raise ValueError("Unknown density metric")
        if self.detector not in ("sample", "positive_peak", "negative_peak", "rms"):
            raise ValueError("Unknown display detector")
        for value in (self.range_db, self.persistence_seconds, self.average_seconds,
                      self.waterfall_row_seconds, self.max_density_history_mb):
            if not math.isfinite(value) or value <= 0:
                raise ValueError("Times, ranges and memory limits must be positive")
        if self.persistence_frames < 1 or not 16 <= self.density_levels <= 256:
            raise ValueError("Invalid frame count/density level count")
        if not self.density_max_db > self.density_min_db:
            raise ValueError("Invalid density level range")
        if not 1 <= self.waterfall_rows <= 1000:
            raise ValueError("Waterfall rows must be in 1..1000")


class Analyzer:
    """All valid analysis FFT frames contribute, independently of snapshots."""
    def __init__(self, config: AnalysisConfig | None = None):
        self._lock = threading.RLock()
        self.config = replace(config or AnalysisConfig())
        self._unit_values = {}
        self.reset()

    def reset(self, config=None):
        with self._lock:
            if config is not None:
                self.config = replace(config)
            cfg = self.config
            cfg.__post_init__()
            self.hop = max(1, int(round(cfg.fft_size*(1-cfg.overlap))))
            self.bin_hz = cfg.sample_rate/cfg.fft_size
            self._window = window_values(cfg.window, cfg.fft_size)
            self.enbw_hz = float(cfg.sample_rate*np.sum(self._window**2)/np.sum(self._window)**2)
            self.frequency_offsets_hz = np.fft.fftshift(np.fft.fftfreq(cfg.fft_size, 1/cfg.sample_rate))
            self._psd_scale = cfg.sample_rate*np.sum(self._window**2)
            self._tail = np.empty(0, np.complex64)
            self._tail_start = None
            self._expected_next = None
            self.last_psd = np.zeros(cfg.fft_size)
            self._max_psd = np.zeros(cfg.fft_size)
            self._min_psd = np.full(cfg.fft_size, np.inf)
            self._avg_acc = np.zeros(cfg.fft_size)
            self._avg_weight = 0.0
            shape = (cfg.fft_size, cfg.density_levels)
            self._cumulative = np.zeros(shape, np.uint64)
            self._exponential = np.zeros(shape)
            self._window_hist = np.zeros(shape, np.int32)
            self._window_entries = deque()
            self._window_bytes = 0
            self._window_truncated = False
            self._window_last_forced_drop = None
            self._last_levels = None
            self._exp_denominator = 0.0
            self._last_frame_time = None
            self.total_valid_frames = 0
            self.invalid_frames = 0
            self.gap_samples = 0
            self._waterfall = deque(maxlen=cfg.waterfall_rows)
            self._waterfall_times = deque(maxlen=cfg.waterfall_rows)
            self._waterfall_bucket = None
            self._waterfall_sum = np.zeros(cfg.fft_size)
            self._waterfall_count = 0
            self._cell_offsets = np.arange(cfg.fft_size, dtype=np.int64)*cfg.density_levels

    @property
    def average_psd(self):
        return self._avg_acc/max(self._avg_weight, 1e-30)

    def _tau(self):
        cfg = self.config
        # Frames designate a half-life measured in analysis-hop intervals.
        if cfg.persistence_unit == "frames":
            return cfg.persistence_frames*self.hop/cfg.sample_rate/math.log(2)
        if cfg.persistence_unit == "time_constant":
            return cfg.persistence_seconds
        return cfg.persistence_seconds/math.log(2)

    def update_config(self, config: AnalysisConfig):
        """Change display settings without clearing FFT statistics unnecessarily.

        Actual FFT/input/grid changes always clear all history. Retain policy
        keeps each unit's entered value and clears Persistence history when the
        unit changes. Convert policy preserves effective time and history.
        """
        with self._lock:
            config = replace(config)
            config.__post_init__()
            old = self.config
            keys = ("sample_rate", "fft_size", "window", "overlap", "density_levels",
                    "density_min_db", "density_max_db")
            if any(getattr(old, key) != getattr(config, key) for key in keys):
                self.reset(config)
                return {"history_cleared": True, "reason": "analysis_conditions_changed"}
            switched = old.persistence_unit != config.persistence_unit
            self._unit_values[old.persistence_unit] = (old.persistence_seconds, old.persistence_frames)
            if switched and config.switch_policy == "convert":
                tau = self._tau()
                if config.persistence_unit == "time_constant":
                    config.persistence_seconds = tau
                elif config.persistence_unit == "half_life":
                    config.persistence_seconds = tau*math.log(2)
                else:
                    config.persistence_frames = max(1, round(tau*math.log(2)*config.sample_rate/self.hop))
            elif switched and config.persistence_unit in self._unit_values:
                config.persistence_seconds, config.persistence_frames = self._unit_values[config.persistence_unit]
            clear = switched and config.switch_policy in ("retain", "clear")
            if config.persistence_mode != old.persistence_mode:
                clear = True
            self.config = config
            if clear:
                self._cumulative.fill(0)
                self.total_valid_frames = 0
                self._exponential.fill(0)
                self._window_hist.fill(0)
                self._window_entries.clear()
                self._window_bytes = 0
                self._exp_denominator = 0
                self._last_levels = None
                self._window_truncated = False
                self._window_last_forced_drop = None
            else:
                self._prune_window_history()
            return {"history_cleared": clear, "reason": "persistence_setting_changed" if clear else None}

    configure = update_config

    def note_gap(self, samples):
        with self._lock:
            self.gap_samples += max(0, int(samples))
            self._tail = np.empty(0, np.complex64)
            self._tail_start = None
            self._expected_next = None
            self._waterfall.append(np.full(self.config.fft_size, np.nan, np.float32))
            self._waterfall_times.append(None)
            self._waterfall_bucket = None
            self._waterfall_sum.fill(0)
            self._waterfall_count = 0

    def _hist(self, levels, weights=None):
        cells = self._cell_offsets[None, :] + levels
        if weights is not None:
            weights = np.repeat(weights, self.config.fft_size)
        result = np.bincount(cells.ravel(), weights=weights,
                             minlength=self.config.fft_size*self.config.density_levels)
        return result.reshape(self.config.fft_size, self.config.density_levels)

    def _prune_window_history(self):
        """Expire the requested window first; only then apply the memory cap."""
        self._window_truncated = False
        if not self._window_entries and self._last_frame_time is None:
            return
        cfg = self.config
        latest = self._window_entries[-1][0] if self._window_entries else self._last_frame_time
        horizon = (cfg.persistence_frames*self.hop/cfg.sample_rate
                   if cfg.persistence_unit == "frames" else cfg.persistence_seconds)
        cap = int(cfg.max_density_history_mb*1024*1024)
        expired = []
        while self._window_entries:
            is_expired = (len(self._window_entries) > cfg.persistence_frames
                          if cfg.persistence_unit == "frames"
                          else self._window_entries[0][0] <= latest-horizon+1e-12)
            if not is_expired and self._window_bytes <= cap:
                break
            if not is_expired and self._window_bytes > cap:
                self._window_last_forced_drop = (self._window_entries[0][0],
                                                self.total_valid_frames-len(self._window_entries)+1)
            _, row = self._window_entries.popleft()
            self._window_bytes -= row.nbytes
            expired.append(row)
        if expired:
            self._window_hist -= self._hist(np.stack(expired)).astype(np.int32)
        if self._window_last_forced_drop is not None:
            time, sequence = self._window_last_forced_drop
            self._window_truncated = (self.total_valid_frames-sequence < cfg.persistence_frames
                                      if cfg.persistence_unit == "frames"
                                      else time > latest-horizon+1e-12)

    def process(self, iq, start_sample=0):
        with self._lock:
            cfg = self.config
            iq = np.asarray(iq, dtype=np.complex64)
            if iq.ndim != 1:
                raise ValueError("IQ must be a one-dimensional complex array")
            start_sample = int(start_sample)
            if self._expected_next is not None and start_sample != self._expected_next:
                self.note_gap(max(0, start_sample-self._expected_next))
            self._expected_next = start_sample+len(iq)
            if self._tail_start is None:
                self._tail_start = start_sample
            self._tail = np.concatenate((self._tail, iq))
            count = max(0, (len(self._tail)-cfg.fft_size)//self.hop+1)
            empty = np.empty((0, cfg.fft_size))
            result = {"frames_processed": 0, "last_psd": self.last_psd,
                      "fft_frame_psd": empty, "fft_frame_end_samples": np.empty(0, np.int64),
                      "frame_end_samples": np.empty(0, np.int64),
                      "start_sample": start_sample, "end_sample": start_sample+len(iq),
                      "gap_samples": self.gap_samples}
            if not count:
                return result
            frames = np.lib.stride_tricks.sliding_window_view(self._tail, cfg.fft_size)[::self.hop][:count]
            ends = self._tail_start + np.arange(count, dtype=np.int64)*self.hop+cfg.fft_size
            valid = np.isfinite(frames).all(axis=1)
            self.invalid_frames += int(count-np.count_nonzero(valid))
            frames = frames[valid]
            ends = ends[valid]
            offset = count*self.hop
            self._tail = self._tail[offset:].copy()
            self._tail_start += offset
            if not len(frames):
                return result
            spectra = np.fft.fftshift(np.fft.fft(frames*self._window, axis=1), axes=1)
            psds = np.abs(spectra)**2/self._psd_scale
            times = ends/cfg.sample_rate
            latest = float(times[-1])
            self.last_psd = psds[-1].copy()
            self._max_psd = np.maximum(self._max_psd, np.max(psds, axis=0))
            self._min_psd = np.minimum(self._min_psd, np.min(psds, axis=0))
            elapsed = (latest-self._last_frame_time) if self._last_frame_time is not None else 0.0
            avg_decay = math.exp(-max(0, elapsed)/cfg.average_seconds)
            avg_weights = np.exp(-(latest-times)/cfg.average_seconds)
            self._avg_acc = self._avg_acc*avg_decay + np.sum(psds*avg_weights[:, None], axis=0)
            self._avg_weight = self._avg_weight*avg_decay + float(np.sum(avg_weights))
            levels = np.floor((power_db(psds*self.enbw_hz)-cfg.density_min_db)
                              *cfg.density_levels/(cfg.density_max_db-cfg.density_min_db))
            levels = levels.clip(0, cfg.density_levels-1).astype(np.uint8)
            if self.total_valid_frames+len(frames) >= 2**63:
                raise OverflowError("Density hit counter limit reached; clear history")
            batch_hist = self._hist(levels).astype(np.int32)
            self._cumulative += batch_hist.astype(np.uint64)
            self.total_valid_frames += len(frames)
            tau = self._tau()
            decay = math.exp(-max(0, elapsed)/tau)
            weights = np.exp(-(latest-times)/tau)
            self._exponential *= decay
            self._exponential += self._hist(levels, weights)
            self._exp_denominator = self._exp_denominator*decay+float(weights.sum())
            self._last_levels = levels[-1].copy()
            self._window_hist += batch_hist
            for time, row in zip(times, levels):
                copied = row.copy()
                self._window_entries.append((float(time), copied))
                self._window_bytes += copied.nbytes
            self._last_frame_time = latest
            self._prune_window_history()
            for time, psd in zip(times, psds):
                bucket = int(math.floor(time/cfg.waterfall_row_seconds))
                if self._waterfall_bucket is not None and bucket != self._waterfall_bucket:
                    if self._waterfall_count:
                        self._waterfall.append((self._waterfall_sum/self._waterfall_count).astype(np.float32))
                        self._waterfall_times.append((self._waterfall_bucket+.5)*cfg.waterfall_row_seconds)
                    self._waterfall_sum.fill(0)
                    self._waterfall_count = 0
                self._waterfall_bucket = bucket
                self._waterfall_sum += psd
                self._waterfall_count += 1
            result.update(frames_processed=len(frames), last_psd=self.last_psd,
                          fft_frame_psd=psds, fft_frame_end_samples=ends,
                          frame_end_samples=ends,
                          peak_dbfs=float(power_db(np.max(self.last_psd)*self.enbw_hz)),
                          integrated_dbfs=float(power_db(np.sum(self.last_psd)*self.bin_hz)))
            return result

    def _persistence_hist(self):
        mode = self.config.persistence_mode.lower()
        if mode == "infinite":
            return self._cumulative, float(self.total_valid_frames)
        if mode in ("window", "time_window"):
            return self._window_hist, float(len(self._window_entries))
        if mode == "off":
            if self._last_levels is None:
                return np.zeros_like(self._exponential), 0.0
            return self._hist(self._last_levels[None, :]), 1.0
        return self._exponential, self._exp_denominator

    def density_statistics(self):
        with self._lock:
            hist, denominator = self._persistence_hist()
            if self.config.stats_mode == "cumulative":
                hist, denominator = self._cumulative, float(self.total_valid_frames)
            return hist.copy(), denominator

    @staticmethod
    def _groups(indices, width):
        count = min(max(1, int(width)), len(indices))
        boundaries = np.linspace(0, len(indices), count+1).astype(int)
        return [indices[a:b] for a, b in zip(boundaries[:-1], boundaries[1:])]

    def _reduce_trace(self, values, groups):
        detector = self.config.detector
        sizes = np.asarray([len(group) for group in groups])
        starts = np.concatenate(([0], np.cumsum(sizes[:-1])))
        chosen = values[np.concatenate(groups)]
        if detector == "sample":
            return chosen[starts+sizes//2]
        if detector == "negative_peak":
            return np.minimum.reduceat(chosen, starts)
        if detector == "rms":
            return np.add.reduceat(chosen, starts)/sizes
        return np.maximum.reduceat(chosen, starts)

    def snapshot(self, span_hz=None, center_hz=0, width=768):
        with self._lock:
            cfg = self.config
            span = min(cfg.sample_rate, float(span_hz or cfg.sample_rate))
            if span <= 0 or not math.isfinite(span):
                raise ValueError("Span must be positive")
            indices = np.flatnonzero(np.abs(self.frequency_offsets_hz) <= span/2)
            if not len(indices):
                indices = np.array([cfg.fft_size//2])
            groups = self._groups(indices, width)
            sizes = np.asarray([len(group) for group in groups])
            starts = np.concatenate(([0], np.cumsum(sizes[:-1])))
            freq = (np.add.reduceat(self.frequency_offsets_hz[indices], starts)/sizes+center_hz).tolist()
            spectrum = {"f_hz": freq, "unit": "dBFS", "bin_hz": self.bin_hz,
                        "enbw_hz": self.enbw_hz, "detector": cfg.detector,
                        "level_reference": "two-sided PSD times window ENBW"}
            for key, power in (("live_db", self.last_psd), ("max_db", self._max_psd),
                               ("min_db", np.where(np.isfinite(self._min_psd), self._min_psd, 0)),
                               ("avg_db", self.average_psd)):
                spectrum[key] = power_db(self._reduce_trace(power, groups)*self.enbw_hz).tolist()
            display_hist, display_denom = self._persistence_hist()
            stats_hist, stats_denom = self.density_statistics()
            # Drawing aggregation only; the full-resolution raw counts stay intact.
            drawn = (np.add.reduceat(display_hist[indices], starts, axis=0)/sizes[:, None]).T[::-1]
            is_rate = cfg.density_metric == "rate"
            if is_rate:
                drawn = drawn/max(display_denom, 1)
            top = max(float(np.max(drawn)), 1e-30)
            image = np.rint(255*np.sqrt(np.clip(drawn/top, 0, 1))).astype(np.uint8)
            stats_values = stats_hist/max(stats_denom, 1) if is_rate else stats_hist
            limit_bytes = int(cfg.max_density_history_mb*1024*1024)
            capacity_frames = limit_bytes//cfg.fft_size
            requested_seconds = (cfg.persistence_frames*self.hop/cfg.sample_rate
                                 if cfg.persistence_unit == "frames" else cfg.persistence_seconds)
            requested_frames = (cfg.persistence_frames if cfg.persistence_unit == "frames"
                                else int(math.ceil(requested_seconds*cfg.sample_rate/self.hop-1e-12)))
            retained_seconds = 0.0
            if self._window_entries:
                retained_seconds = min(requested_seconds,
                    max(0.0, self._window_entries[-1][0]-self._window_entries[0][0]+self.hop/cfg.sample_rate))
            density = {"width": len(groups), "height": cfg.density_levels,
                       "data_b64": base64.b64encode(image.tobytes()).decode("ascii"),
                       "min_db": cfg.density_min_db, "max_db": cfg.density_max_db,
                       "metric": cfg.density_metric, "stats_mode": cfg.stats_mode,
                       "normalization": "relative square-root color; rates use all valid weighted analysis FFTs",
                       "valid_frames": self.total_valid_frames, "effective_frames": stats_denom,
                       "display_effective_frames": display_denom,
                       "maximum_value": float(np.max(stats_values)),
                       "weighted_hits": cfg.stats_mode == "persistence" and cfg.persistence_mode == "exponential",
                       "window_truncated": self._window_truncated,
                       "window_frames_retained": len(self._window_entries),
                       "window_history_bytes": self._window_bytes,
                       "window_seconds_retained": retained_seconds,
                       "window_history_limit_bytes": limit_bytes,
                       "window_capacity_frames": capacity_frames,
                       "window_capacity_seconds": capacity_frames*self.hop/cfg.sample_rate,
                       "window_requested_seconds": requested_seconds,
                       "window_requested_frames": requested_frames,
                       "window_memory_limit_expected": requested_frames > capacity_frames,
                       "window_history_limit_scope": "level-index payload; excludes fixed histograms and container overhead",
                       "orientation": "high_level_at_top"}
            waterfall_rows = list(reversed(self._waterfall))
            if self._waterfall_count:
                waterfall_rows.insert(0, self._waterfall_sum/self._waterfall_count)
            waterfall_rows = waterfall_rows[:cfg.waterfall_rows]
            if waterfall_rows:
                wf = np.add.reduceat(np.stack(waterfall_rows)[:, indices], starts, axis=1)/sizes
                gap_rows = np.isnan(wf).any(axis=1)
                wf_db = power_db(np.nan_to_num(wf, nan=0)*self.enbw_hz)
                wf_image = np.rint(255*np.clip((wf_db-(cfg.ref_level-cfg.range_db))/cfg.range_db, 0, 1)).astype(np.uint8)
                wf_image[gap_rows] = 0
            else:
                wf_image = np.zeros((1, len(groups)), np.uint8)
                gap_rows = np.zeros(1, bool)
            waterfall = {"width": len(groups), "height": len(wf_image),
                         "data_b64": base64.b64encode(wf_image.tobytes()).decode("ascii"),
                         "min_db": cfg.ref_level-cfg.range_db, "max_db": cfg.ref_level,
                         "row_seconds": cfg.waterfall_row_seconds,
                         "gap_rows": np.flatnonzero(gap_rows).tolist(), "orientation": "newest_at_top"}
            return {"spectrum": spectrum, "density": density, "waterfall": waterfall,
                    "valid_frames": self.total_valid_frames, "invalid_frames": self.invalid_frames,
                    "gap_samples": self.gap_samples, "bin_hz": self.bin_hz,
                    "enbw_hz": self.enbw_hz}

    def measurements(self, band_low_hz, band_high_hz, center_hz=0,
                     obw_percent=99, marker_count=6):
        with self._lock:
            low, high, center = map(float, (band_low_hz, band_high_hz, center_hz))
            if not 0 < float(obw_percent) < 100:
                raise ValueError("OBW percentage must be between 0 and 100")
            unavailable = {"status": "unavailable", "cp_dbfs": None, "cp_avg_dbfs": None,
                           "obw_hz": None, "obw_low_hz": None, "obw_high_hz": None,
                           "markers": [], "unit": "dBFS"}
            if self._last_frame_time is None:
                return dict(unavailable, reason="No analysis frames")
            if not math.isfinite(low+high+center) or low >= high or low < center-self.config.sample_rate/2 or high > center+self.config.sample_rate/2:
                return dict(unavailable, reason="Measurement band outside acquired band")
            frequency = self.frequency_offsets_hz+center
            edges_low = np.maximum(frequency-self.bin_hz/2, low)
            edges_high = np.minimum(frequency+self.bin_hz/2, high)
            widths = np.maximum(0, edges_high-edges_low)
            selected = np.flatnonzero(widths > 0)
            powers = self.last_psd[selected]*widths[selected]
            total = float(powers.sum())
            average = float(np.sum(self.average_psd*widths))
            if total <= 0:
                return dict(unavailable, reason="No finite power in measurement band")
            cumulative = np.cumsum(powers)
            def quantile(fraction):
                target = total*fraction
                j = min(int(np.searchsorted(cumulative, target)), len(selected)-1)
                previous = cumulative[j-1] if j else 0
                part = (target-previous)/max(powers[j], 1e-300)
                idx = selected[j]
                return float(edges_low[idx]+np.clip(part, 0, 1)*widths[idx])
            tail = (1-float(obw_percent)/100)/2
            left, right = quantile(tail), quantile(1-tail)
            p = self.last_psd
            local = np.flatnonzero((p >= np.roll(p, 1)) & (p > np.roll(p, -1)) & (widths > 0))
            noise = float(np.median(p[selected]))
            # Suppress arithmetic roundoff peaks as well as noise-floor ripples.
            local = local[p[local] > max(noise*4, float(np.max(p[selected]))*1e-12, 1e-29)]
            ordered = local[np.argsort(-p[local], kind="stable")]
            chosen = []
            for idx in ordered:
                if all(abs(int(idx)-j) >= 2 for j in chosen):
                    chosen.append(int(idx))
                if len(chosen) >= max(0, int(marker_count)):
                    break
            if marker_count <= 0:
                chosen = []
            markers = []
            for rank, idx in enumerate(chosen, 1):
                correction = 0.0
                if 0 < idx < len(p)-1:
                    yy = np.log(np.maximum(p[idx-1:idx+2], 1e-300))
                    divisor = yy[0]-2*yy[1]+yy[2]
                    if abs(divisor) > 1e-15:
                        correction = float(np.clip(.5*(yy[0]-yy[2])/divisor, -.5, .5))
                marker = {"rank": rank, "frequency_hz": float(frequency[idx]+correction*self.bin_hz),
                          "level_dbfs": float(power_db(p[idx]*self.enbw_hz))}
                if markers:
                    marker["delta_hz"] = marker["frequency_hz"]-markers[0]["frequency_hz"]
                    marker["delta_db"] = marker["level_dbfs"]-markers[0]["level_dbfs"]
                else:
                    marker.update(delta_hz=0.0, delta_db=0.0)
                markers.append(marker)
            return {"status": "ok", "cp_dbfs": float(power_db(total)),
                    "cp_avg_dbfs": float(power_db(average)),
                    "obw_hz": right-left, "obw_low_hz": left, "obw_high_hz": right,
                    "obw_percent": float(obw_percent), "markers": markers, "unit": "dBFS",
                    "marker_min_separation_bins": 2, "marker_threshold_db_above_median": 10*math.log10(4)}


class _Fir:
    """Streaming FFT convolution, retaining exactly the required prior samples."""
    def __init__(self, taps):
        self.taps = np.asarray(taps, np.float32)
        self.history = np.zeros(len(self.taps)-1, np.float32)
        self.fft_length = 1 << (max(4096, len(self.taps)*8)-1).bit_length()
        self._response = np.fft.fft(self.taps, self.fft_length)

    def process(self, values):
        values = np.asarray(values)
        if not len(values):
            return values.copy()
        start = len(self.taps)-1
        stride = self.fft_length-start
        pieces = []
        for offset in range(0, len(values), stride):
            part = values[offset:offset+stride]
            ext = np.concatenate((self.history, part))
            convolved = np.fft.ifft(np.fft.fft(ext, self.fft_length)*self._response)
            pieces.append(convolved[start:start+len(part)])
            self.history = ext[-start:].copy() if start else np.empty(0)
        out = np.concatenate(pieces)
        return out if np.iscomplexobj(values) else out.real


def _lowpass(sample_rate, cutoff, length):
    length = max(17, int(length) | 1)
    n = np.arange(length)-(length-1)/2
    taps = 2*cutoff/sample_rate*np.sinc(2*cutoff/sample_rate*n)*np.hamming(length)
    return taps/np.sum(taps)


class _OnePole:
    def __init__(self, sample_rate, seconds):
        self.a = math.exp(-1/(sample_rate*seconds))
        self.state = 0.0
        self.chunk = max(1, min(4096, int(20/max(-math.log(self.a), 1e-12))))

    def process(self, values):
        values = np.asarray(values, np.float64)
        out = np.empty_like(values)
        for start in range(0, len(values), self.chunk):
            x = values[start:start+self.chunk]
            powers = self.a**np.arange(1, len(x)+1)
            y = powers*(self.state+(1-self.a)*np.cumsum(x/powers))
            out[start:start+len(x)] = y
            self.state = float(y[-1])
        return out


class VfoDemodulator:
    """Stateful DDC, antialias filtering, demodulation and 32 kHz mono audio."""
    def __init__(self, sample_rate, frequency_offset_hz=0, bandwidth_hz=200_000,
                 mode="WFM", audio_rate=32_000):
        self.sample_rate = float(sample_rate)
        self.frequency_offset_hz = float(frequency_offset_hz)
        self.bandwidth_hz = float(bandwidth_hz)
        self.mode = str(mode).upper()
        self.audio_rate = int(audio_rate)
        if self.mode not in ("AM", "NFM", "WFM"):
            raise ValueError("Supported demodulation modes are AM, NFM and WFM")
        if not (self.sample_rate > 0 and self.audio_rate > 0 and 0 < self.bandwidth_hz < self.sample_rate):
            raise ValueError("Invalid sample rate or demodulation bandwidth")
        if abs(self.frequency_offset_hz)+self.bandwidth_hz/2 > self.sample_rate/2:
            raise ValueError("Demodulation filter outside acquired band")
        self.reset()

    def reset(self):
        target_rate = max(self.bandwidth_hz*1.25, self.audio_rate*4)
        self.factor = max(1, int(self.sample_rate/target_rate))
        self.channel_rate = self.sample_rate/self.factor
        transition = max(500, min(self.bandwidth_hz*.25, self.channel_rate/2-self.bandwidth_hz/2))
        taps = min(4095, max(65, int(4*self.sample_rate/transition)))
        self._channel_filter = _Fir(_lowpass(self.sample_rate, self.bandwidth_hz/2, taps))
        audio_cutoff = min(self.audio_rate*.45, 15_000 if self.mode == "WFM" else min(5_000, self.bandwidth_hz*.4))
        audio_taps = min(1023, max(65, int(4*self.channel_rate/max(1000, self.audio_rate/2-audio_cutoff))))
        self._audio_filter = _Fir(_lowpass(self.channel_rate, audio_cutoff, audio_taps))
        self._dc = _OnePole(self.channel_rate, .05)
        self._deemphasis = _OnePole(self.audio_rate, 50e-6) if self.mode == "WFM" else None
        self._phase = 0.0
        self._input_count = 0
        self._previous_iq = None
        self._channel_count = 0
        self._output_count = 0
        self._previous_audio = None

    def process(self, iq):
        iq = np.asarray(iq, dtype=np.complex64)
        if iq.ndim != 1 or not np.isfinite(iq).all():
            raise ValueError("Demodulation IQ must be finite and one dimensional")
        if not len(iq):
            return np.empty(0, np.float32)
        omega = -2*np.pi*self.frequency_offset_hz/self.sample_rate
        mixed = (iq*np.exp(1j*(self._phase+omega*np.arange(len(iq))))).astype(np.complex64)
        self._phase = float((self._phase+omega*len(iq)) % (2*np.pi))
        filtered = self._channel_filter.process(mixed)
        offset = (-self._input_count) % self.factor
        channel = filtered[offset::self.factor]
        self._input_count += len(iq)
        if not len(channel):
            return np.empty(0, np.float32)
        if self.mode == "AM":
            envelope = np.abs(channel)
            dc = self._dc.process(envelope)
            audio = (envelope-dc)/np.maximum(dc, 1e-4)
        else:
            previous = np.concatenate(([self._previous_iq if self._previous_iq is not None else channel[0]], channel[:-1]))
            difference = np.angle(channel*np.conj(previous))
            self._previous_iq = channel[-1]
            deviation = 75_000 if self.mode == "WFM" else min(5_000, self.bandwidth_hz*.2)
            audio = difference*self.channel_rate/(2*np.pi*deviation)
            audio -= self._dc.process(audio)
        audio = self._audio_filter.process(audio)
        start = self._channel_count
        end = start+len(audio)-1
        step = self.channel_rate/self.audio_rate
        next_time = self._output_count*step
        number = max(0, int(math.floor((end-next_time)/step+1e-9))+1)
        times = (self._output_count+np.arange(number))*step-start
        if self._previous_audio is None:
            result = np.interp(times, np.arange(len(audio)), audio)
        else:
            result = np.interp(times, np.arange(-1, len(audio)), np.concatenate(([self._previous_audio], audio)))
        self._previous_audio = audio[-1]
        self._channel_count += len(audio)
        self._output_count += number
        if self._deemphasis is not None:
            result = self._deemphasis.process(result)
        # Preserve out-of-range values for the PCM writer's clipping counter.
        # Limiting here would hide negative saturation from the recording log.
        return (result*demod_audio_scale(self.mode)).astype(np.float32)
