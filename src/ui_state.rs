use std::sync::Arc;

use truce_egui::ParamState;

use crate::core::{SpectrumData, DB_FLOOR};
use crate::registry::{self, InstanceId};
use crate::shmem::{self, SpectrumSource};

// ---------------------------------------------------------------------------
// View mode
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Both = 0,
    Normal = 1,
    Diff = 2,
}

impl ViewMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Normal,
            2 => Self::Diff,
            _ => Self::Both,
        }
    }
}

// ---------------------------------------------------------------------------
// Remote spectrum cache
// ---------------------------------------------------------------------------

pub struct RemoteCache {
    pub id: InstanceId,
    pub spectrum: Arc<dyn SpectrumSource>,
    pub bins: Vec<f32>,
    pub last_version: u32,
    pub diff_bins: Vec<f32>,
    diff_local_version: u32,
    diff_remote_version: u32,
}

// ---------------------------------------------------------------------------
// Persistent state (serialized via #[derive(State)])
// ---------------------------------------------------------------------------

use truce::prelude::*;

#[derive(State, Default, Clone)]
pub struct PersistentState {
    pub instance_name: String,
    pub selected_remote_names: Vec<String>,
    pub view_mode: u8,
}

// ---------------------------------------------------------------------------
// UiState — all mutable GUI state
// ---------------------------------------------------------------------------

pub struct UiState {
    // Local spectrum
    pub spectrum: Arc<SpectrumData>,
    pub bins_a: Vec<f32>,
    pub bins_b: Vec<f32>,
    pub last_version: u32,

    // Remote sources
    pub selected_ids: Vec<InstanceId>,
    pub remotes: Vec<RemoteCache>,

    // View
    pub view_mode: ViewMode,

    // Instance identity
    pub instance_id: InstanceId,
    pub instance_name: String,
    pub editing_name: bool,
}

impl UiState {
    pub fn new(
        spectrum: Arc<SpectrumData>,
        instance_id: InstanceId,
    ) -> Self {
        let num_bins = spectrum.num_bins();
        let instance_name = registry::name_of(instance_id).unwrap_or_default();

        Self {
            spectrum,
            bins_a: vec![DB_FLOOR; num_bins],
            bins_b: vec![DB_FLOOR; num_bins],
            last_version: 0,
            selected_ids: Vec::new(),
            remotes: Vec::new(),
            view_mode: ViewMode::Both,
            instance_id,
            instance_name,
            editing_name: false,
        }
    }

    /// Apply state from the plugin (called on open and on state_changed).
    pub fn apply_state(&mut self, state: &ParamState) {
        let data = state.get_state();
        if data.is_empty() {
            return;
        }
        let Some(ps) = PersistentState::deserialize(&data) else {
            return;
        };
        if !ps.instance_name.is_empty() && !self.editing_name {
            self.instance_name = ps.instance_name;
        }
        self.view_mode = ViewMode::from_u8(ps.view_mode);
        self.selected_ids = ps
            .selected_remote_names
            .iter()
            .filter_map(|name| registry::find_by_name(name))
            .collect();
        self.spectrum
            .set_has_remotes(!self.selected_ids.is_empty());
    }

    /// Write current UI state back to the plugin.
    pub fn sync_to_plugin(&self, state: &ParamState) {
        self.spectrum
            .set_has_remotes(!self.selected_ids.is_empty());
        let ps = PersistentState {
            instance_name: self.instance_name.clone(),
            selected_remote_names: self
                .selected_ids
                .iter()
                .filter_map(|id| registry::name_of(*id))
                .collect(),
            view_mode: self.view_mode as u8,
        };
        state.set_state(ps.serialize());
    }

    /// Read local spectrum if version changed.
    pub fn update_local(&mut self) {
        let version = self.spectrum.version();
        if version != self.last_version {
            self.last_version = version;
            self.spectrum.read_all(&mut self.bins_a);
            if self.spectrum.is_both() {
                self.spectrum.read_all_b(&mut self.bins_b);
            }
        }
    }

    /// Refresh remote caches.
    pub fn update_remotes(&mut self) {
        self.remotes.retain(|r| self.selected_ids.contains(&r.id));

        for &id in &self.selected_ids {
            if self.remotes.iter().any(|r| r.id == id) {
                continue;
            }
            let source: Option<Arc<dyn SpectrumSource>> = registry::get(id)
                .map(|s| s as Arc<dyn SpectrumSource>)
                .or_else(|| {
                    shmem::open_shared_spectrum(id.0)
                        .map(|s| s as Arc<dyn SpectrumSource>)
                });
            if let Some(spectrum) = source {
                let num_bins = spectrum.num_bins();
                self.remotes.push(RemoteCache {
                    id,
                    spectrum,
                    bins: vec![DB_FLOOR; num_bins],
                    last_version: 0,
                    diff_bins: vec![0.0; num_bins],
                    diff_local_version: 0,
                    diff_remote_version: 0,
                });
            }
        }

        for remote in &mut self.remotes {
            let v = remote.spectrum.version();
            if v != remote.last_version {
                remote.last_version = v;
                remote.spectrum.read_all(&mut remote.bins);
            }
        }
    }

    /// Recompute diffs for all remotes (version-matched).
    pub fn update_diff(&mut self) {
        let local_v = self.last_version;
        for remote in &mut self.remotes {
            if local_v > remote.diff_local_version
                && remote.last_version > remote.diff_remote_version
            {
                remote.diff_local_version = local_v;
                remote.diff_remote_version = remote.last_version;
                let n = self.bins_a.len().min(remote.bins.len()).min(remote.diff_bins.len());
                for i in 0..n {
                    remote.diff_bins[i] = self.bins_a[i] - remote.bins[i];
                }
            }
        }
    }

    /// Toggle a remote source on/off.
    pub fn toggle_source(&mut self, id: InstanceId, state: &ParamState) {
        if let Some(pos) = self.selected_ids.iter().position(|&x| x == id) {
            self.selected_ids.remove(pos);
        } else {
            self.selected_ids.push(id);
        }
        self.sync_to_plugin(state);
    }

    /// Change view mode.
    pub fn set_view_mode(&mut self, mode: ViewMode, state: &ParamState) {
        self.view_mode = mode;
        self.sync_to_plugin(state);
    }
}
