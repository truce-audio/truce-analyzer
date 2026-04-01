use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use crate::core::SpectrumData;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct InstanceId(pub u32);

struct InstanceEntry {
    name: String,
    spectrum: Arc<SpectrumData>,
}

#[derive(Default)]
struct RegistryInner {
    instances: HashMap<u32, InstanceEntry>,
    next_id: u32,
}

static REGISTRY: LazyLock<Mutex<RegistryInner>> = LazyLock::new(Mutex::default);

pub fn register(name: Option<&str>, spectrum: Arc<SpectrumData>) -> InstanceId {
    let mut reg = REGISTRY.lock().unwrap();
    reg.next_id += 1;
    let id = reg.next_id;
    let name = name
        .map(String::from)
        .unwrap_or_else(|| format!("Analyzer {id}"));
    reg.instances.insert(id, InstanceEntry { name, spectrum });
    InstanceId(id)
}

pub fn deregister(id: InstanceId) {
    REGISTRY.lock().unwrap().instances.remove(&id.0);
}

pub fn list() -> Vec<(InstanceId, String)> {
    REGISTRY
        .lock()
        .unwrap()
        .instances
        .iter()
        .map(|(&id, e)| (InstanceId(id), e.name.clone()))
        .collect()
}

pub fn get(id: InstanceId) -> Option<Arc<SpectrumData>> {
    REGISTRY
        .lock()
        .unwrap()
        .instances
        .get(&id.0)
        .map(|e| e.spectrum.clone())
}

pub fn rename(id: InstanceId, new_name: &str) {
    if let Some(entry) = REGISTRY.lock().unwrap().instances.get_mut(&id.0) {
        entry.name = new_name.to_string();
    }
}

pub fn find_by_name(name: &str) -> Option<InstanceId> {
    REGISTRY
        .lock()
        .unwrap()
        .instances
        .iter()
        .find(|(_, e)| e.name == name)
        .map(|(&id, _)| InstanceId(id))
}

pub fn name_of(id: InstanceId) -> Option<String> {
    REGISTRY
        .lock()
        .unwrap()
        .instances
        .get(&id.0)
        .map(|e| e.name.clone())
}
