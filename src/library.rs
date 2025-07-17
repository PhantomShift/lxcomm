use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: u32,
    pub title: String,
    pub path: PathBuf,
    pub needs_update: bool,
    #[serde(skip)]
    pub selected: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LibraryItemSettings {
    pub enabled: bool,
}

impl Default for LibraryItemSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Default)]
pub struct Library {
    inner: BTreeMap<u32, LibraryItem>,
}

impl Library {
    pub fn iter_selected(&self) -> impl Iterator<Item = &LibraryItem> {
        self.inner.values().filter(|item| item.selected)
    }
}

impl AsRef<BTreeMap<u32, LibraryItem>> for Library {
    fn as_ref(&self) -> &BTreeMap<u32, LibraryItem> {
        &self.inner
    }
}

impl AsMut<BTreeMap<u32, LibraryItem>> for Library {
    fn as_mut(&mut self) -> &mut BTreeMap<u32, LibraryItem> {
        &mut self.inner
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Profile {
    pub id: usize,
    pub name: String,
    pub items: BTreeMap<u32, LibraryItemSettings>,

    #[serde(skip)]
    pub add_selected: bool,
    #[serde(skip)]
    pub view_selected_item: Option<u32>,
}

impl PartialEq for Profile {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl PartialOrd for Profile {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Profile {
    pub fn missing_items(&self, library: &Library) -> Vec<u32> {
        let mut missing = Vec::new();
        for (item, _) in self.items.iter() {
            if !library.as_ref().contains_key(item) {
                missing.push(*item);
            }
        }

        missing
    }
}
