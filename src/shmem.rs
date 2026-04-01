use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::core::{SpectrumData, BINS_PER_OCTAVE, CQT_F_MIN};

// ---------------------------------------------------------------------------
// SpectrumSource — read-only interface for local or remote spectrum data
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub trait SpectrumSource: Send + Sync {
    fn num_bins(&self) -> usize;
    fn center_freq(&self, bin: usize) -> f32;
    fn center_freqs_slice(&self) -> &[f32];
    fn read_all(&self, out: &mut [f32]);
    fn read_all_b(&self, out: &mut [f32]);
    fn mode(&self) -> u8;
    fn version(&self) -> u32;
    fn is_both(&self) -> bool;
    fn nearest_bin(&self, freq: f32) -> usize;
}

impl SpectrumSource for SpectrumData {
    fn num_bins(&self) -> usize {
        self.num_bins()
    }
    fn center_freq(&self, bin: usize) -> f32 {
        self.center_freq(bin)
    }
    fn center_freqs_slice(&self) -> &[f32] {
        self.center_freqs_slice()
    }
    fn read_all(&self, out: &mut [f32]) {
        self.read_all(out);
    }
    fn read_all_b(&self, out: &mut [f32]) {
        self.read_all_b(out);
    }
    fn mode(&self) -> u8 {
        self.mode()
    }
    fn version(&self) -> u32 {
        self.version()
    }
    fn is_both(&self) -> bool {
        self.is_both()
    }
    fn nearest_bin(&self, freq: f32) -> usize {
        self.nearest_bin(freq)
    }
}

// ---------------------------------------------------------------------------
// Shared memory layout constants
// ---------------------------------------------------------------------------

const SHM_MAGIC: u32 = 0x5441_5A52; // "TAZR"
const SHM_VERSION: u32 = 1;
const SHM_NAME_MAX: usize = 64;

// Header: magic(4) + version(4) + num_bins(4) + sample_rate(4) + mode(4)
//       + data_version(4) + name_len(4) + name(64) + reserved(4) = 96 bytes
const SHM_HEADER_SIZE: usize = 96;

fn shm_total_size(num_bins: usize) -> usize {
    SHM_HEADER_SIZE + num_bins * 4 * 2 // bins_a + bins_b
}

fn shm_name_for_id(id: u32) -> String {
    format!("/truce-analyzer-{id}")
}

// ---------------------------------------------------------------------------
// SharedMemoryWriter — owning side, creates and updates the region
// ---------------------------------------------------------------------------

pub struct SharedMemoryWriter {
    ptr: *mut u8,
    size: usize,
    num_bins: usize,
    #[cfg(unix)]
    shm_name: String,
}

unsafe impl Send for SharedMemoryWriter {}
unsafe impl Sync for SharedMemoryWriter {}

impl SharedMemoryWriter {
    pub fn create(instance_id: u32, name: &str, num_bins: usize) -> Option<Self> {
        let size = shm_total_size(num_bins);
        let shm_name = shm_name_for_id(instance_id);

        #[cfg(unix)]
        {
            let ptr = unsafe { create_shm_unix(&shm_name, size) }?;
            let writer = Self {
                ptr,
                size,
                num_bins,
                shm_name,
            };
            // Write header
            writer.write_u32(0, SHM_MAGIC);
            writer.write_u32(4, SHM_VERSION);
            writer.write_u32(8, num_bins as u32);
            writer.write_u32(16, 0); // mode
            writer.write_u32(20, 0); // data_version
            // Write name
            let name_bytes = name.as_bytes();
            let name_len = name_bytes.len().min(SHM_NAME_MAX);
            writer.write_u32(24, name_len as u32);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    name_bytes.as_ptr(),
                    writer.ptr.add(28),
                    name_len,
                );
            }
            Some(writer)
        }

        #[cfg(not(unix))]
        {
            let _ = (size, shm_name, name);
            None // Windows support deferred
        }
    }

    pub fn update(&self, spectrum: &SpectrumData) {
        // Write sample_rate
        self.write_u32(12, spectrum.sample_rate_bits());
        // Write mode
        self.write_u32(16, spectrum.mode() as u32);
        // Write version
        self.write_u32(20, spectrum.version());
        // Write bins_a
        let bins_a_offset = SHM_HEADER_SIZE;
        for i in 0..self.num_bins {
            self.write_u32(bins_a_offset + i * 4, spectrum.read_bin_bits(i));
        }
        // Write bins_b
        let bins_b_offset = SHM_HEADER_SIZE + self.num_bins * 4;
        for i in 0..self.num_bins {
            self.write_u32(bins_b_offset + i * 4, spectrum.read_bin_b_bits(i));
        }
    }

    #[allow(dead_code)]
    pub fn update_name(&self, name: &str) {
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(SHM_NAME_MAX);
        self.write_u32(24, name_len as u32);
        unsafe {
            // Zero the name area first
            std::ptr::write_bytes(self.ptr.add(28), 0, SHM_NAME_MAX);
            std::ptr::copy_nonoverlapping(name_bytes.as_ptr(), self.ptr.add(28), name_len);
        }
    }

    fn write_u32(&self, offset: usize, value: u32) {
        unsafe {
            let atom = &*(self.ptr.add(offset) as *const AtomicU32);
            atom.store(value, Ordering::Relaxed);
        }
    }
}

impl Drop for SharedMemoryWriter {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.size);
            let c_name = std::ffi::CString::new(self.shm_name.as_str()).unwrap();
            libc::shm_unlink(c_name.as_ptr());
        }
    }
}

// ---------------------------------------------------------------------------
// SharedMemorySpectrum — reader side, opens an existing region
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct SharedMemorySpectrum {
    ptr: *const u8,
    size: usize,
    num_bins: usize,
    center_freqs: Vec<f32>,
}

unsafe impl Send for SharedMemorySpectrum {}
unsafe impl Sync for SharedMemorySpectrum {}

#[allow(dead_code)]
pub fn open_shared_spectrum(instance_id: u32) -> Option<Arc<SharedMemorySpectrum>> {
    let shm_name = shm_name_for_id(instance_id);

    #[cfg(unix)]
    {
        let ptr = unsafe { open_shm_unix(&shm_name) }?;

        // Read header
        let magic = read_u32(ptr, 0);
        if magic != SHM_MAGIC {
            unsafe { libc::munmap(ptr as *mut libc::c_void, SHM_HEADER_SIZE) };
            return None;
        }
        let num_bins = read_u32(ptr, 8) as usize;
        let size = shm_total_size(num_bins);

        // Recompute center frequencies (must match the writer's CQT params)
        let center_freqs: Vec<f32> = (0..num_bins)
            .map(|k| CQT_F_MIN * 2.0_f32.powf(k as f32 / BINS_PER_OCTAVE as f32))
            .collect();

        Some(Arc::new(SharedMemorySpectrum {
            ptr,
            size,
            num_bins,
            center_freqs,
        }))
    }

    #[cfg(not(unix))]
    {
        let _ = shm_name;
        None
    }
}

impl SharedMemorySpectrum {
    fn read_u32(&self, offset: usize) -> u32 {
        read_u32(self.ptr, offset)
    }

    fn read_bin_at(&self, base_offset: usize, index: usize) -> f32 {
        f32::from_bits(self.read_u32(base_offset + index * 4))
    }
}

impl SpectrumSource for SharedMemorySpectrum {
    fn num_bins(&self) -> usize {
        self.num_bins
    }

    fn center_freq(&self, bin: usize) -> f32 {
        self.center_freqs[bin]
    }

    fn center_freqs_slice(&self) -> &[f32] {
        &self.center_freqs
    }

    fn read_all(&self, out: &mut [f32]) {
        let base = SHM_HEADER_SIZE;
        for (i, v) in out.iter_mut().enumerate().take(self.num_bins) {
            *v = self.read_bin_at(base, i);
        }
    }

    fn read_all_b(&self, out: &mut [f32]) {
        let base = SHM_HEADER_SIZE + self.num_bins * 4;
        for (i, v) in out.iter_mut().enumerate().take(self.num_bins) {
            *v = self.read_bin_at(base, i);
        }
    }

    fn mode(&self) -> u8 {
        self.read_u32(16) as u8
    }

    fn version(&self) -> u32 {
        self.read_u32(20)
    }

    fn is_both(&self) -> bool {
        self.mode() == crate::core::MODE_BOTH
    }

    fn nearest_bin(&self, freq: f32) -> usize {
        if freq <= CQT_F_MIN {
            return 0;
        }
        let k = BINS_PER_OCTAVE as f32 * (freq / CQT_F_MIN).log2();
        (k.round() as usize).min(self.num_bins.saturating_sub(1))
    }
}

impl Drop for SharedMemorySpectrum {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.size);
        }
    }
}

// ---------------------------------------------------------------------------
// Platform helpers
// ---------------------------------------------------------------------------

fn read_u32(ptr: *const u8, offset: usize) -> u32 {
    unsafe {
        let atom = &*(ptr.add(offset) as *const AtomicU32);
        atom.load(Ordering::Relaxed)
    }
}

#[cfg(unix)]
unsafe fn create_shm_unix(name: &str, size: usize) -> Option<*mut u8> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let fd = libc::shm_open(
        c_name.as_ptr(),
        libc::O_CREAT | libc::O_RDWR,
        0o600,
    );
    if fd < 0 {
        return None;
    }
    if libc::ftruncate(fd, size as libc::off_t) != 0 {
        libc::close(fd);
        libc::shm_unlink(c_name.as_ptr());
        return None;
    }
    let ptr = libc::mmap(
        std::ptr::null_mut(),
        size,
        libc::PROT_READ | libc::PROT_WRITE,
        libc::MAP_SHARED,
        fd,
        0,
    );
    libc::close(fd);
    if ptr == libc::MAP_FAILED {
        libc::shm_unlink(c_name.as_ptr());
        return None;
    }
    Some(ptr as *mut u8)
}

#[cfg(unix)]
unsafe fn open_shm_unix(name: &str) -> Option<*const u8> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let fd = libc::shm_open(c_name.as_ptr(), libc::O_RDONLY, 0);
    if fd < 0 {
        return None;
    }
    // Read just the header first to get num_bins
    let ptr = libc::mmap(
        std::ptr::null_mut(),
        SHM_HEADER_SIZE,
        libc::PROT_READ,
        libc::MAP_SHARED,
        fd,
        0,
    );
    if ptr == libc::MAP_FAILED {
        libc::close(fd);
        return None;
    }
    let num_bins = read_u32(ptr as *const u8, 8) as usize;
    let full_size = shm_total_size(num_bins);
    libc::munmap(ptr, SHM_HEADER_SIZE);

    // Remap at full size
    let ptr = libc::mmap(
        std::ptr::null_mut(),
        full_size,
        libc::PROT_READ,
        libc::MAP_SHARED,
        fd,
        0,
    );
    libc::close(fd);
    if ptr == libc::MAP_FAILED {
        return None;
    }
    Some(ptr as *const u8)
}

// ---------------------------------------------------------------------------
// File-based cross-process registry
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
pub struct FileRegistry {
    pub instances: Vec<FileRegistryEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileRegistryEntry {
    pub id: u32,
    pub name: String,
    pub shm_name: String,
    pub pid: u32,
}

impl FileRegistry {
    fn path() -> std::path::PathBuf {
        #[cfg(target_os = "macos")]
        {
            let home = std::env::var("HOME").unwrap_or_default();
            std::path::PathBuf::from(home)
                .join("Library/Application Support/TruceAnalyzer/registry.json")
        }
        #[cfg(target_os = "linux")]
        {
            let home = std::env::var("HOME").unwrap_or_default();
            std::path::PathBuf::from(home)
                .join(".local/share/TruceAnalyzer/registry.json")
        }
        #[cfg(target_os = "windows")]
        {
            let appdata = std::env::var("APPDATA").unwrap_or_default();
            std::path::PathBuf::from(appdata).join("TruceAnalyzer/registry.json")
        }
    }

    pub fn load() -> Self {
        let path = Self::path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }

    pub fn add(&mut self, id: u32, name: &str) {
        self.remove_stale_pids();
        self.instances.push(FileRegistryEntry {
            id,
            name: name.to_string(),
            shm_name: shm_name_for_id(id),
            pid: std::process::id(),
        });
        self.save();
    }

    pub fn remove(&mut self, id: u32) {
        self.instances.retain(|e| e.id != id);
        self.save();
    }

    fn remove_stale_pids(&mut self) {
        self.instances.retain(|e| is_pid_alive(e.pid));
    }
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true // Assume alive on unsupported platforms
    }
}
