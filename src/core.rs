use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

// ---------------------------------------------------------------------------
// CQT parameters
// ---------------------------------------------------------------------------

pub const BINS_PER_OCTAVE: usize = 48;
pub const CQT_F_MIN: f32 = 27.5; // A0
pub const CQT_F_MAX: f32 = 20480.0;

pub const DB_FLOOR: f32 = -90.0;
pub const DB_CEIL: f32 = 0.0;
pub const FREQ_MIN: f32 = 20.0; // display range lower bound
pub const FREQ_MAX: f32 = 20_000.0;

const SMOOTH_UP: f32 = 0.4;
const SMOOTH_DOWN: f32 = 0.8;
const HOP_SIZE: usize = 2048;
const KERNEL_SPARSITY_THRESHOLD: f32 = 0.001;

// ---------------------------------------------------------------------------
// Channel mode constants (matches ChannelMode enum order in lib.rs)
// ---------------------------------------------------------------------------

pub const MODE_BOTH: u8 = 0;
pub const MODE_LEFT: u8 = 1;
pub const MODE_RIGHT: u8 = 2;
pub const MODE_SUM: u8 = 3;
pub const MODE_DIFF: u8 = 4;

/// Compute center frequencies for all CQT bins.
pub fn cqt_center_frequencies() -> Vec<f32> {
    let num_octaves = (CQT_F_MAX / CQT_F_MIN).log2();
    let num_bins = (BINS_PER_OCTAVE as f32 * num_octaves).ceil() as usize;
    (0..num_bins)
        .map(|k| CQT_F_MIN * 2.0_f32.powf(k as f32 / BINS_PER_OCTAVE as f32))
        .collect()
}

/// The constant Q factor for the configured bins-per-octave.
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
            mode: AtomicU8::new(MODE_BOTH),
        }
    }

    pub fn num_bins(&self) -> usize {
        self.center_freqs.len()
    }

    pub fn center_freq(&self, bin: usize) -> f32 {
        self.center_freqs[bin]
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

    /// Find the CQT bin nearest to a given frequency (O(1) for log-spaced bins).
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
            let window_len = (q * sample_rate / freq as f64).ceil() as usize;
            let window_len = window_len.min(fft_size);
            let inv_n = 1.0 / window_len as f64;

            // Time-domain kernel, right-aligned so it analyses the most recent
            // window_len samples in the signal buffer (newest samples are at the end).
            let mut kernel = vec![Complex::new(0.0f32, 0.0); fft_size];
            let offset = fft_size - window_len;
            for n in 0..window_len {
                let hann = 0.5 * (1.0 - (pi2 * n as f64 * inv_n).cos());
                let phase = -pi2 * freq as f64 * n as f64 / sample_rate;
                kernel[offset + n] = Complex::new(
                    (hann * phase.cos() * inv_n) as f32,
                    (hann * phase.sin() * inv_n) as f32,
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

pub struct AnalyzerCore {
    spectrum: Arc<SpectrumData>,

    // Reconfigured on reset(); kernels generated lazily on first run_cqt()
    sample_rate: f64,
    fft_size: usize,
    fft: Option<Arc<dyn Fft<f32>>>,
    kernels: Vec<SparseKernel>,
    kernels_stale: bool,
    signal_buf: Vec<Complex<f32>>,
    ring_left: Vec<f32>,
    ring_right: Vec<f32>,
    ring_pos: usize,
    hop_counter: usize,
    smoothed_a: Vec<f32>,
    smoothed_b: Vec<f32>,
}

impl AnalyzerCore {
    pub fn new(spectrum: Arc<SpectrumData>) -> Self {
        let num_bins = spectrum.num_bins();
        Self {
            spectrum,
            sample_rate: 0.0,
            fft_size: 0,
            fft: None,
            kernels: Vec::new(),
            kernels_stale: true,
            signal_buf: Vec::new(),
            ring_left: Vec::new(),
            ring_right: Vec::new(),
            ring_pos: 0,
            hop_counter: 0,
            smoothed_a: vec![DB_FLOOR; num_bins],
            smoothed_b: vec![DB_FLOOR; num_bins],
        }
    }

    pub fn reset(&mut self, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.spectrum.set_sample_rate(sample_rate as f32);

        let q = q_factor();
        let max_window = (q * sample_rate / CQT_F_MIN as f64).ceil() as usize;
        self.fft_size = max_window.next_power_of_two();

        // Allocate buffers but defer kernel generation to first run_cqt().
        // This keeps reset() fast, avoiding timeouts in hosts like Pro Tools (AAX).
        self.signal_buf = vec![Complex::new(0.0, 0.0); self.fft_size];
        self.ring_left = vec![0.0; self.fft_size];
        self.ring_right = vec![0.0; self.fft_size];
        self.ring_pos = 0;
        self.hop_counter = 0;
        self.smoothed_a.fill(DB_FLOOR);
        self.smoothed_b.fill(DB_FLOOR);
        self.fft = None;
        self.kernels.clear();
        self.kernels_stale = true;
    }

    /// Generate FFT plan and CQT kernels. Called lazily from the audio thread
    /// on the first hop after reset(), so hosts never block on init.
    fn ensure_kernels(&mut self) {
        if !self.kernels_stale {
            return;
        }
        self.kernels_stale = false;

        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(self.fft_size);
        self.kernels = generate_kernels(
            &self.spectrum.center_freqs,
            self.sample_rate,
            self.fft_size,
            &fft,
        );
        self.fft = Some(fft);
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
        self.ensure_kernels();
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
    }

    pub fn spectrum(&self) -> &Arc<SpectrumData> {
        &self.spectrum
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

pub fn format_freq(freq: f32) -> String {
    if freq >= 1000.0 {
        format!("{}k", (freq / 1000.0) as u32)
    } else {
        format!("{}", freq as u32)
    }
}
