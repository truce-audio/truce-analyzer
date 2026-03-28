use std::sync::atomic::{AtomicU32, Ordering};
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
    bins: Box<[AtomicU32]>,
    center_freqs: Box<[f32]>,
    sample_rate: AtomicU32,
}

impl SpectrumData {
    pub fn new(center_freqs: Vec<f32>) -> Self {
        let bins: Vec<AtomicU32> = (0..center_freqs.len())
            .map(|_| AtomicU32::new(DB_FLOOR.to_bits()))
            .collect();
        Self {
            bins: bins.into_boxed_slice(),
            center_freqs: center_freqs.into_boxed_slice(),
            sample_rate: AtomicU32::new(44100.0_f32.to_bits()),
        }
    }

    pub fn num_bins(&self) -> usize {
        self.center_freqs.len()
    }

    pub fn center_freq(&self, bin: usize) -> f32 {
        self.center_freqs[bin]
    }

    pub fn write_bin(&self, index: usize, db: f32) {
        self.bins[index].store(db.to_bits(), Ordering::Relaxed);
    }

    pub fn read_bin(&self, index: usize) -> f32 {
        f32::from_bits(self.bins[index].load(Ordering::Relaxed))
    }

    pub fn set_sample_rate(&self, sr: f32) {
        self.sample_rate.store(sr.to_bits(), Ordering::Relaxed);
    }

    pub fn sample_rate(&self) -> f32 {
        f32::from_bits(self.sample_rate.load(Ordering::Relaxed))
    }

    /// Batch-read all bins into a caller-owned slice (avoids per-frame allocation).
    pub fn read_all(&self, out: &mut [f32]) {
        for (i, v) in out.iter_mut().enumerate().take(self.num_bins()) {
            *v = self.read_bin(i);
        }
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

            // Conjugate and keep only significant entries
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

// ---------------------------------------------------------------------------
// AnalyzerCore — CQT processor running on the audio thread
// ---------------------------------------------------------------------------

pub struct AnalyzerCore {
    spectrum: Arc<SpectrumData>,

    // Reconfigured on reset() (sample-rate dependent)
    fft_size: usize,
    fft: Option<Arc<dyn Fft<f32>>>,
    kernels: Vec<SparseKernel>,
    signal_buf: Vec<Complex<f32>>,
    ring_buffer: Vec<f32>,
    ring_pos: usize,
    hop_counter: usize,
    smoothed: Vec<f32>,
}

impl AnalyzerCore {
    pub fn new(spectrum: Arc<SpectrumData>) -> Self {
        let num_bins = spectrum.num_bins();
        Self {
            spectrum,
            fft_size: 0,
            fft: None,
            kernels: Vec::new(),
            signal_buf: Vec::new(),
            ring_buffer: Vec::new(),
            ring_pos: 0,
            hop_counter: 0,
            smoothed: vec![DB_FLOOR; num_bins],
        }
    }

    pub fn reset(&mut self, sample_rate: f64) {
        self.spectrum.set_sample_rate(sample_rate as f32);

        // Determine FFT size from the longest CQT window (lowest frequency bin)
        let q = q_factor();
        let max_window = (q * sample_rate / CQT_F_MIN as f64).ceil() as usize;
        self.fft_size = max_window.next_power_of_two();

        // Build FFT plan and CQT kernels
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(self.fft_size);
        self.kernels = generate_kernels(
            &self.spectrum.center_freqs,
            sample_rate,
            self.fft_size,
            &fft,
        );
        self.fft = Some(fft);

        // Allocate buffers
        self.signal_buf = vec![Complex::new(0.0, 0.0); self.fft_size];
        self.ring_buffer = vec![0.0; self.fft_size];
        self.ring_pos = 0;
        self.hop_counter = 0;
        self.smoothed.fill(DB_FLOOR);
    }

    pub fn process_sample(&mut self, sample: f32) {
        if self.fft_size == 0 {
            return; // not yet initialized
        }

        self.ring_buffer[self.ring_pos] = sample;
        self.ring_pos = (self.ring_pos + 1) % self.fft_size;
        self.hop_counter += 1;

        if self.hop_counter >= HOP_SIZE {
            self.hop_counter = 0;
            self.run_cqt();
        }
    }

    fn run_cqt(&mut self) {
        let fft = match &self.fft {
            Some(f) => f.clone(),
            None => return,
        };

        // Copy ring buffer to signal_buf in chronological order
        for i in 0..self.fft_size {
            let idx = (self.ring_pos + i) % self.fft_size;
            self.signal_buf[i] = Complex::new(self.ring_buffer[idx], 0.0);
        }

        // Forward FFT of the signal
        fft.process(&mut self.signal_buf);

        // Sparse dot product for each CQT bin
        for (q, kernel) in self.kernels.iter().enumerate() {
            let mut sum = Complex::new(0.0f32, 0.0);
            for &(k, coeff) in &kernel.entries {
                sum += self.signal_buf[k] * coeff;
            }
            let magnitude = sum.norm() / self.fft_size as f32;
            let db = (20.0 * magnitude.max(1e-10).log10()).clamp(DB_FLOOR, DB_CEIL);

            let alpha = if db > self.smoothed[q] {
                SMOOTH_UP
            } else {
                SMOOTH_DOWN
            };
            self.smoothed[q] += alpha * (db - self.smoothed[q]);
            self.spectrum.write_bin(q, self.smoothed[q]);
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
