use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use truce_core::cast::{param_f32, sample_count_usize, sample_f32};
use truce_dsp::AudioTapConsumer;

use crate::shmem::SharedMemoryWriter;

// ---------------------------------------------------------------------------
// CQT parameters
// ---------------------------------------------------------------------------

pub const BINS_PER_OCTAVE: usize = 96;
pub const CQT_F_MIN: f32 = 27.5; // A0
pub const CQT_F_MAX: f32 = 20480.0;

pub const DB_FLOOR: f32 = -90.0;
pub const DB_CEIL: f32 = 0.0;
pub const FREQ_MIN: f32 = 20.0; // display range lower bound
pub const FREQ_MAX: f32 = 20_000.0;

const SMOOTH_UP: f32 = 0.9;
const SMOOTH_DOWN: f32 = 0.9;
const HOP_SIZE: usize = 2048;
const KERNEL_SPARSITY_THRESHOLD: f32 = 0.001;

// ---------------------------------------------------------------------------
// Channel mode constants (matches ChannelMode enum order in lib.rs)
// ---------------------------------------------------------------------------

pub const MODE_SUM: u8 = 0;
pub const MODE_BOTH: u8 = 1;
pub const MODE_LEFT: u8 = 2;
pub const MODE_RIGHT: u8 = 3;
pub const MODE_DIFF: u8 = 4;

/// Compute center frequencies for all CQT bins.
///
/// `BINS_PER_OCTAVE * num_octaves` is bounded by the audible-range
/// configuration (≲ 1k bins), so the f32 ↔ usize round trip is exact.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "CQT bin count is bounded by audible-range config; f32 / usize round-trip is exact"
)]
pub fn cqt_center_frequencies() -> Vec<f32> {
    let num_octaves = (CQT_F_MAX / CQT_F_MIN).log2();
    let num_bins = (BINS_PER_OCTAVE as f32 * num_octaves).ceil() as usize;
    (0..num_bins)
        .map(|k| CQT_F_MIN * 2.0_f32.powf(k as f32 / BINS_PER_OCTAVE as f32))
        .collect()
}

/// The constant Q factor for the configured bins-per-octave.
#[allow(
    clippy::cast_precision_loss,
    reason = "BINS_PER_OCTAVE is the small const 96; fits in f64 mantissa exactly"
)]
fn q_factor() -> f64 {
    1.0 / (2.0_f64.powf(1.0 / BINS_PER_OCTAVE as f64) - 1.0)
}

// ---------------------------------------------------------------------------
// SpectrumData — lock-free shared state between audio and GUI threads
// ---------------------------------------------------------------------------

pub struct SpectrumData {
    bins_a: Box<[AtomicU32]>,
    bins_b: Box<[AtomicU32]>,
    center_freqs: Box<[f32]>,
    sample_rate: AtomicU32,
    mode: AtomicU8,
    version: AtomicU32,
    has_remotes: AtomicBool,
}

fn make_bins(n: usize) -> Box<[AtomicU32]> {
    (0..n)
        .map(|_| AtomicU32::new(DB_FLOOR.to_bits()))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

impl SpectrumData {
    pub fn new(center_freqs: Vec<f32>) -> Self {
        let n = center_freqs.len();
        Self {
            bins_a: make_bins(n),
            bins_b: make_bins(n),
            center_freqs: center_freqs.into_boxed_slice(),
            sample_rate: AtomicU32::new(44100.0_f32.to_bits()),
            mode: AtomicU8::new(MODE_SUM),
            version: AtomicU32::new(0),
            has_remotes: AtomicBool::new(false),
        }
    }

    pub fn num_bins(&self) -> usize {
        self.center_freqs.len()
    }

    #[allow(dead_code)]
    pub fn center_freq(&self, bin: usize) -> f32 {
        self.center_freqs[bin]
    }

    pub fn center_freqs_slice(&self) -> &[f32] {
        &self.center_freqs
    }

    pub fn write_bin(&self, index: usize, db: f32) {
        self.bins_a[index].store(db.to_bits(), Ordering::Relaxed);
    }

    pub fn read_bin(&self, index: usize) -> f32 {
        f32::from_bits(self.bins_a[index].load(Ordering::Relaxed))
    }

    pub fn write_bin_b(&self, index: usize, db: f32) {
        self.bins_b[index].store(db.to_bits(), Ordering::Relaxed);
    }

    pub fn read_bin_b(&self, index: usize) -> f32 {
        f32::from_bits(self.bins_b[index].load(Ordering::Relaxed))
    }

    pub fn set_sample_rate(&self, sr: f32) {
        self.sample_rate.store(sr.to_bits(), Ordering::Relaxed);
    }

    pub fn sample_rate_bits(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    pub fn read_bin_bits(&self, index: usize) -> u32 {
        self.bins_a[index].load(Ordering::Relaxed)
    }

    pub fn read_bin_b_bits(&self, index: usize) -> u32 {
        self.bins_b[index].load(Ordering::Relaxed)
    }

    pub fn read_all(&self, out: &mut [f32]) {
        for (i, v) in out.iter_mut().enumerate().take(self.num_bins()) {
            *v = self.read_bin(i);
        }
    }

    pub fn read_all_b(&self, out: &mut [f32]) {
        for (i, v) in out.iter_mut().enumerate().take(self.num_bins()) {
            *v = self.read_bin_b(i);
        }
    }

    pub fn set_mode(&self, mode: u8) {
        self.mode.store(mode, Ordering::Relaxed);
    }

    pub fn mode(&self) -> u8 {
        self.mode.load(Ordering::Relaxed)
    }

    pub fn is_both(&self) -> bool {
        self.mode() == MODE_BOTH
    }

    pub fn set_has_remotes(&self, v: bool) {
        self.has_remotes.store(v, Ordering::Relaxed);
    }

    pub fn has_remotes(&self) -> bool {
        self.has_remotes.load(Ordering::Relaxed)
    }

    pub fn version(&self) -> u32 {
        self.version.load(Ordering::Relaxed)
    }

    pub fn bump_version(&self) {
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// Find the CQT bin nearest to a given frequency (O(1) for log-spaced bins).
    ///
    /// `k` is non-negative (we early-out when `freq <= CQT_F_MIN`) and
    /// bounded by `num_bins`, so the round/cast pair is lossless.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "k is in [0, num_bins) by the early-return guard"
    )]
    pub fn nearest_bin(&self, freq: f32) -> usize {
        if freq <= CQT_F_MIN {
            return 0;
        }
        let k = BINS_PER_OCTAVE as f32 * (freq / CQT_F_MIN).log2();
        (k.round() as usize).min(self.num_bins().saturating_sub(1))
    }
}

// ---------------------------------------------------------------------------
// Sparse CQT kernel (frequency-domain, conjugated)
// ---------------------------------------------------------------------------

struct SparseKernel {
    entries: Vec<(usize, Complex<f32>)>,
}

/// Pre-compute sparse frequency-domain kernels for all CQT bins.
///
/// Uses the Brown-Puckette method: each time-domain kernel is a Hann-windowed
/// complex exponential at the bin's center frequency, FFT'd and sparsified.
/// At runtime a single signal FFT + sparse dot products yields all CQT bins.
///
/// `n` and `window_len` are both bounded by `fft_size` (≤ 2²⁰), well
/// within `f64`'s 52-bit mantissa, so the int → float casts inside
/// the inner loop are exact.
#[allow(
    clippy::cast_precision_loss,
    reason = "kernel time index n stays within the FFT window length"
)]
fn generate_kernels(
    center_freqs: &[f32],
    sample_rate: f64,
    fft_size: usize,
    fft: &Arc<dyn Fft<f32>>,
) -> Vec<SparseKernel> {
    let q = q_factor();
    let pi2 = 2.0 * std::f64::consts::PI;
    let scratch_len = fft.get_inplace_scratch_len();
    let mut scratch = vec![Complex::new(0.0f32, 0.0); scratch_len];

    center_freqs
        .iter()
        .map(|&freq| {
            let window_len = sample_count_usize(q * sample_rate / f64::from(freq)).min(fft_size);
            let inv_n = 1.0 / window_len as f64;

            // Time-domain kernel, right-aligned so it analyses the most recent
            // window_len samples in the signal buffer (newest samples are at the end).
            let mut kernel = vec![Complex::new(0.0f32, 0.0); fft_size];
            let offset = fft_size - window_len;
            for n in 0..window_len {
                let hann = 0.5 * (1.0 - (pi2 * n as f64 * inv_n).cos());
                let phase = -pi2 * f64::from(freq) * n as f64 / sample_rate;
                kernel[offset + n] = Complex::new(
                    sample_f32(hann * phase.cos() * inv_n),
                    sample_f32(hann * phase.sin() * inv_n),
                );
            }

            fft.process_with_scratch(&mut kernel, &mut scratch);

            let max_mag = kernel.iter().map(|c| c.norm()).fold(0.0f32, f32::max);
            let threshold = max_mag * KERNEL_SPARSITY_THRESHOLD;

            let entries: Vec<(usize, Complex<f32>)> = kernel
                .iter()
                .enumerate()
                .filter(|(_, c)| c.norm() > threshold)
                .map(|(k, c)| (k, c.conj()))
                .collect();

            SparseKernel { entries }
        })
        .collect()
}

/// Apply sparse CQT dot products to an already-FFT'd signal buffer.
#[allow(
    clippy::cast_precision_loss,
    reason = "fft_size is bounded by audible sample-rate / CQT_F_MIN; well within f32 precision"
)]
fn apply_cqt(
    signal: &[Complex<f32>],
    kernels: &[SparseKernel],
    smoothed: &mut [f32],
    fft_size: usize,
) {
    for (q, kernel) in kernels.iter().enumerate() {
        let mut sum = Complex::new(0.0f32, 0.0);
        for &(k, coeff) in &kernel.entries {
            sum += signal[k] * coeff;
        }
        let magnitude = sum.norm() / fft_size as f32;
        let db = (20.0 * magnitude.max(1e-10).log10()).clamp(DB_FLOOR, DB_CEIL);

        let alpha = if db > smoothed[q] {
            SMOOTH_UP
        } else {
            SMOOTH_DOWN
        };
        smoothed[q] += alpha * (db - smoothed[q]);
    }
}

// ---------------------------------------------------------------------------
// AnalyzerCore — CQT processor running on the audio thread
// ---------------------------------------------------------------------------

struct KernelData {
    kernels: Vec<SparseKernel>,
    fft: Arc<dyn Fft<f32>>,
}

pub struct AnalyzerCore {
    spectrum: Arc<SpectrumData>,

    fft_size: usize,
    fft: Option<Arc<dyn Fft<f32>>>,
    kernels: Vec<SparseKernel>,
    kernel_rx: Option<mpsc::Receiver<KernelData>>,
    signal_buf: Vec<Complex<f32>>,
    ring_left: Vec<f32>,
    ring_right: Vec<f32>,
    ring_pos: usize,
    hop_counter: usize,
    smoothed_a: Vec<f32>,
    smoothed_b: Vec<f32>,
    shmem_writer: Option<SharedMemoryWriter>,
}

impl AnalyzerCore {
    pub fn new(spectrum: Arc<SpectrumData>) -> Self {
        let num_bins = spectrum.num_bins();
        Self {
            spectrum,
            fft_size: 0,
            fft: None,
            kernels: Vec::new(),
            kernel_rx: None,
            signal_buf: Vec::new(),
            ring_left: Vec::new(),
            ring_right: Vec::new(),
            ring_pos: 0,
            hop_counter: 0,
            smoothed_a: vec![DB_FLOOR; num_bins],
            smoothed_b: vec![DB_FLOOR; num_bins],
            shmem_writer: None,
        }
    }

    pub fn set_shmem_writer(&mut self, writer: SharedMemoryWriter) {
        self.shmem_writer = Some(writer);
    }

    pub fn reset(&mut self, sample_rate: f64) {
        self.spectrum.set_sample_rate(param_f32(sample_rate));

        let q = q_factor();
        let max_window = sample_count_usize(q * sample_rate / f64::from(CQT_F_MIN));
        let new_fft_size = max_window.next_power_of_two();

        if new_fft_size == self.fft_size {
            // Same sample rate — keep existing kernels and smoothed state.
            // Only clear ring buffers (stale audio). Smoothed arrays retain
            // their values so the display doesn't flash on transport resets
            // (AU hosts call reset() on play/stop/seek).
            self.ring_left.fill(0.0);
            self.ring_right.fill(0.0);
        } else {
            // Sample rate changed — reallocate buffers, generate kernels on
            // a background thread so neither init nor audio thread blocks.
            self.fft_size = new_fft_size;
            self.signal_buf = vec![Complex::new(0.0, 0.0); self.fft_size];
            self.ring_left = vec![0.0; self.fft_size];
            self.ring_right = vec![0.0; self.fft_size];
            self.fft = None;
            self.kernels.clear();

            let (tx, rx) = mpsc::channel();
            self.kernel_rx = Some(rx);
            let spectrum = self.spectrum.clone();
            let fft_size = self.fft_size;
            std::thread::spawn(move || {
                let mut planner = FftPlanner::new();
                let fft = planner.plan_fft_forward(fft_size);
                let kernels = generate_kernels(&spectrum.center_freqs, sample_rate, fft_size, &fft);
                let _ = tx.send(KernelData { kernels, fft });
            });
            self.smoothed_a.fill(DB_FLOOR);
            self.smoothed_b.fill(DB_FLOOR);
        }

        self.ring_pos = 0;
        self.hop_counter = 0;
    }

    /// Block until background kernel generation completes (for tests).
    #[allow(dead_code)]
    pub fn wait_for_kernels(&mut self) {
        if let Some(rx) = self.kernel_rx.take() {
            if let Ok(data) = rx.recv() {
                self.kernels = data.kernels;
                self.fft = Some(data.fft);
            }
        }
    }

    pub fn process_stereo(&mut self, left: f32, right: f32) {
        if self.fft_size == 0 {
            return;
        }

        self.ring_left[self.ring_pos] = left;
        self.ring_right[self.ring_pos] = right;
        self.ring_pos = (self.ring_pos + 1) % self.fft_size;
        self.hop_counter += 1;

        if self.hop_counter >= HOP_SIZE {
            self.hop_counter = 0;
            self.run_cqt();
        }
    }

    fn run_cqt(&mut self) {
        // Pick up kernels from background thread if ready
        if let Some(rx) = &self.kernel_rx {
            match rx.try_recv() {
                Ok(data) => {
                    self.kernels = data.kernels;
                    self.fft = Some(data.fft);
                    self.kernel_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.kernel_rx = None;
                    return;
                }
            }
        }

        let fft = match &self.fft {
            Some(f) => f.clone(),
            None => return,
        };
        let mode = self.spectrum.mode();

        // Fill signal_buf with the primary derived signal
        for i in 0..self.fft_size {
            let idx = (self.ring_pos + i) % self.fft_size;
            let sample = match mode {
                MODE_RIGHT => self.ring_right[idx],
                MODE_SUM => (self.ring_left[idx] + self.ring_right[idx]) * 0.5,
                MODE_DIFF => (self.ring_left[idx] - self.ring_right[idx]) * 0.5,
                _ => self.ring_left[idx], // Left, Both (primary = L)
            };
            self.signal_buf[i] = Complex::new(sample, 0.0);
        }
        fft.process(&mut self.signal_buf);
        apply_cqt(
            &self.signal_buf,
            &self.kernels,
            &mut self.smoothed_a,
            self.fft_size,
        );
        for (q, &db) in self.smoothed_a.iter().enumerate() {
            self.spectrum.write_bin(q, db);
        }

        // Both mode: also analyse the right channel
        if mode == MODE_BOTH {
            for i in 0..self.fft_size {
                let idx = (self.ring_pos + i) % self.fft_size;
                self.signal_buf[i] = Complex::new(self.ring_right[idx], 0.0);
            }
            fft.process(&mut self.signal_buf);
            apply_cqt(
                &self.signal_buf,
                &self.kernels,
                &mut self.smoothed_b,
                self.fft_size,
            );
            for (q, &db) in self.smoothed_b.iter().enumerate() {
                self.spectrum.write_bin_b(q, db);
            }
        }

        self.spectrum.bump_version();

        if let Some(ref writer) = self.shmem_writer {
            writer.update(&self.spectrum);
        }
    }
}

// ---------------------------------------------------------------------------
// Coordinate mapping helpers
// ---------------------------------------------------------------------------

pub fn freq_to_x(freq: f32, left: f32, width: f32) -> f32 {
    let log_min = FREQ_MIN.ln();
    let log_max = FREQ_MAX.ln();
    let t = (freq.ln() - log_min) / (log_max - log_min);
    left + t * width
}

pub fn db_to_y(db: f32, top: f32, height: f32) -> f32 {
    let t = (db - DB_FLOOR) / (DB_CEIL - DB_FLOOR);
    top + height * (1.0 - t)
}

pub fn x_to_freq(x: f32, left: f32, width: f32) -> f32 {
    let log_min = FREQ_MIN.ln();
    let log_max = FREQ_MAX.ln();
    let t = (x - left) / width;
    (log_min + t * (log_max - log_min)).exp()
}

pub const FREQ_GRID: &[f32] = &[
    50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
];

pub const DB_GRID: &[f32] = &[0.0, -12.0, -24.0, -36.0, -48.0, -60.0, -72.0, -84.0];

/// Display label for a grid frequency. Inputs come from `FREQ_GRID`,
/// which is a fixed positive-finite ladder, so the f32 → u32 truncation
/// is intentional and never lossy.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "FREQ_GRID values are positive integers below u32::MAX"
)]
pub fn format_freq(freq: f32) -> String {
    if freq >= 1000.0 {
        format!("{}k", (freq / 1000.0) as u32)
    } else {
        format!("{}", freq as u32)
    }
}

// ---------------------------------------------------------------------------
// AnalyzerWorker — runs AnalyzerCore on a dedicated thread so the audio
// thread only has to push samples into an AudioTap.
// ---------------------------------------------------------------------------

/// Control channel between the audio thread and the analyzer worker.
///
/// All fields are atomic; the worker polls them each iteration. `reset`
/// is communicated by bumping `reset_version` after writing the new
/// sample rate — the worker calls `AnalyzerCore::reset` when it notices
/// a version change.
struct WorkerCtl {
    sample_rate_bits: AtomicU32,
    reset_version: AtomicU32,
    shutdown: AtomicBool,
}

/// Handle to the background analyzer worker. Dropping the handle
/// requests shutdown and joins the thread.
pub struct AnalyzerWorker {
    ctl: Arc<WorkerCtl>,
    handle: Option<thread::JoinHandle<()>>,
}

impl AnalyzerWorker {
    /// Signal the worker to pick up `sample_rate` and call
    /// `AnalyzerCore::reset` on its next poll.
    pub fn reset(&self, sample_rate: f64) {
        self.ctl
            .sample_rate_bits
            .store(param_f32(sample_rate).to_bits(), Ordering::Relaxed);
        self.ctl.reset_version.fetch_add(1, Ordering::Release);
    }
}

impl Drop for AnalyzerWorker {
    fn drop(&mut self) {
        self.ctl.shutdown.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Pull up to this many frames per drain in `spawn_analyzer_worker`.
/// Bounded so reset / shutdown checks run at least ~1kHz even under
/// heavy input backlog.
const DRAIN_FRAMES: usize = 2048;

/// Spawn the analyzer worker.
///
/// The returned handle owns the worker thread. Pushing samples into
/// `tap_rx`'s paired producer causes the worker to run CQT and write
/// to the shared [`SpectrumData`]. Shared-memory mirroring is optional
/// and, if supplied, is moved into the worker so it drops when the
/// worker thread exits.
pub fn spawn_analyzer_worker(
    spectrum: Arc<SpectrumData>,
    mut tap_rx: AudioTapConsumer,
    shmem_writer: Option<SharedMemoryWriter>,
) -> AnalyzerWorker {
    let ctl = Arc::new(WorkerCtl {
        sample_rate_bits: AtomicU32::new(0),
        reset_version: AtomicU32::new(0),
        shutdown: AtomicBool::new(false),
    });
    let ctl_thread = Arc::clone(&ctl);

    let handle = thread::Builder::new()
        .name("truce-analyzer-worker".into())
        .spawn(move || {
            let mut core = AnalyzerCore::new(spectrum);
            if let Some(writer) = shmem_writer {
                core.set_shmem_writer(writer);
            }

            let channels = tap_rx.channels() as usize;
            debug_assert_eq!(channels, 2, "worker assumes stereo tap");

            let mut scratch = vec![0.0_f32; DRAIN_FRAMES * channels];

            let mut last_reset_version: u32 = 0;

            loop {
                if ctl_thread.shutdown.load(Ordering::Acquire) {
                    break;
                }

                // Pick up any pending reset before draining samples so
                // the new sample rate applies to subsequent audio.
                let v = ctl_thread.reset_version.load(Ordering::Acquire);
                if v != last_reset_version {
                    last_reset_version = v;
                    let sr = f64::from(f32::from_bits(
                        ctl_thread.sample_rate_bits.load(Ordering::Relaxed),
                    ));
                    if sr > 0.0 {
                        core.reset(sr);
                    }
                }

                let frames = tap_rx.read(&mut scratch, DRAIN_FRAMES);
                if frames == 0 {
                    // Nothing to do — sleep briefly. ~4 ms is short
                    // enough that a 60 Hz UI never sees stale data,
                    // long enough to avoid spinning the CPU.
                    thread::sleep(Duration::from_millis(4));
                    continue;
                }

                for f in 0..frames {
                    let l = scratch[f * channels];
                    let r = scratch[f * channels + 1];
                    core.process_stereo(l, r);
                }
            }
        })
        .expect("spawn truce-analyzer-worker");

    AnalyzerWorker {
        ctl,
        handle: Some(handle),
    }
}
