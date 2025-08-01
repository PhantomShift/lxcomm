use std::{collections::BTreeMap, path::PathBuf, sync::LazyLock};

use apply::Apply;
use fuse_rust::Fuse;
use serde::{Deserialize, Serialize};

use crate::{
    files::{Cache, ModDetails},
    xcom_mod::{self, ModId},
};

static DEFAULT_FUZZY_MATCHER: LazyLock<Fuse> = LazyLock::new(|| Fuse {
    // Potential TODO - Adjust this threshold
    threshold: 0.5,
    is_case_sensitive: false,
    tokenize: false,
    ..Default::default()
});

#[derive(Debug, Clone, Default)]
pub enum FilterMethod {
    #[default]
    None,
    Id(String),
    Title(String),
    TitleFuzzy(String),
    Combined(Vec<FilterMethod>),
}

impl FilterMethod {
    pub fn matches(&self, item: &LibraryItem) -> bool {
        match self {
            Self::None => true,
            Self::Id(id_string) => item.id.to_string() == *id_string,
            Self::Title(query) => item.title.to_lowercase().contains(query),
            Self::TitleFuzzy(query) => DEFAULT_FUZZY_MATCHER
                .search_text_in_string(query, &item.title)
                .is_some(),
            Self::Combined(methods) => methods.iter().all(|method| method.matches(item)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryItem {
    pub id: ModId,
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
    pub items: BTreeMap<ModId, LibraryItem>,
    pub compatibility: BTreeMap<ModId, xcom_mod::ModCompatibility>,
    pub filter_fuzzy: bool,
    pub filter_query: String,
    pub missing_dependencies: BTreeMap<ModId, Vec<String>>,
}

impl Library {
    pub fn iter_selected(&self) -> impl Iterator<Item = &LibraryItem> {
        self.items.values().filter(|item| item.selected)
    }
    pub fn iter_workshop_id_selected(&self) -> impl Iterator<Item = u32> {
        self.items.keys().filter_map(ModId::maybe_workshop)
    }

    #[inline]
    pub fn get_filter_method(&self) -> FilterMethod {
        if self.filter_query.is_empty() {
            FilterMethod::None
        } else if self.filter_fuzzy {
            FilterMethod::TitleFuzzy(self.filter_query.clone())
        } else {
            FilterMethod::Title(self.filter_query.clone())
        }
    }

    pub fn iter_with_filter(
        &self,
        filter_method: FilterMethod,
    ) -> impl Iterator<Item = &LibraryItem> {
        self.items
            .values()
            .filter(move |item| filter_method.matches(item))
    }

    pub fn iter_filtered(&self) -> impl Iterator<Item = &LibraryItem> {
        self.iter_with_filter(self.get_filter_method())
    }

    pub fn iter_with_filter_mut(
        &mut self,
        filter_method: FilterMethod,
    ) -> impl Iterator<Item = &mut LibraryItem> {
        self.items
            .values_mut()
            .filter(move |item| filter_method.matches(item))
    }

    pub fn iter_filtered_mut(&mut self) -> impl Iterator<Item = &mut LibraryItem> {
        self.iter_with_filter_mut(self.get_filter_method())
    }

    pub fn update_selected_filtered(&mut self) {
        let method = self.get_filter_method();
        for item in self.items.values_mut() {
            item.selected = item.selected && method.matches(item);
        }
    }

    pub fn update_missing_dependencies(&mut self, cache: Cache) {
        let mut all_missing = BTreeMap::new();
        let provided = xcom_mod::get_provided_mods(self.items.keys(), cache.clone(), self);
        let metadata = cache.get_mod_metadata_list();
        for id in self.items.keys() {
            let mut missing = vec![];
            if let Some(compat) = self.compatibility.get(id) {
                missing.extend(
                    compat
                        .required
                        .iter()
                        .filter(|required| !provided.contains(*required))
                        .cloned(),
                )
            }
            if let Some(ModDetails::Workshop(info)) = cache.get_details(id) {
                missing.extend(info.children.iter().filter_map(|child| {
                    if let Ok(child_id) =
                        child.published_file_id.parse::<u32>().map(ModId::Workshop)
                        && !metadata.iter().any(|entry| {
                            entry.key() == &child_id && provided.contains(&entry.dlc_name)
                        })
                    {
                        Some(format!(
                            "{} ({child_id})",
                            cache
                                .get_details(&child_id)
                                .as_ref()
                                .map(|details| details.title())
                                .unwrap_or("UNKNOWN")
                        ))
                    } else {
                        None
                    }
                }));
            }

            if !missing.is_empty() {
                missing.sort();
                all_missing.insert(id.clone(), missing);
            }
        }

        self.missing_dependencies = all_missing;
    }
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Profile {
    pub id: usize,
    pub name: String,
    pub items: BTreeMap<ModId, LibraryItemSettings>,

    #[serde(skip)]
    pub compatibility_issues: BTreeMap<ModId, Vec<CompatibilityIssue>>,
    #[serde(skip)]
    pub add_selected: bool,
    #[serde(skip)]
    pub view_selected_item: Option<ModId>,
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
    pub fn missing_items(&self, library: &Library) -> Vec<ModId> {
        let mut missing = Vec::new();
        for (item, _) in self.items.iter() {
            if !library.items.contains_key(item) {
                missing.push(item.clone());
            }
        }

        missing
    }

    pub fn update_compatibility_issues(&mut self, cache: Cache, library: &Library) {
        let mut all = BTreeMap::new();
        let provided = xcom_mod::get_provided_mods(self.items.keys(), cache.clone(), library);
        let metadata = cache.get_mod_metadata_list();

        for id in self.items.keys() {
            if let Some(compat) = library.compatibility.get(id) {
                let mut issues = vec![];
                issues.extend(
                    compat
                        .required
                        .iter()
                        .filter(|required| !provided.contains(*required))
                        .cloned()
                        .map(CompatibilityIssue::MissingRequired),
                );

                issues.extend(
                    compat
                        .incompatible
                        .iter()
                        .filter(|incompatible| provided.contains(*incompatible))
                        .cloned()
                        .map(CompatibilityIssue::Incompatible),
                );

                if let Some(data) = cache.get_mod_metadata(id) {
                    issues.extend(self.items.keys().filter_map(|other_id| {
                        if id != other_id {
                            if let Some(other) = cache.get_mod_metadata(other_id)
                                && data.dlc_name == other.dlc_name
                            {
                                Some(CompatibilityIssue::Overlapping(
                                    format!("{} ({})", other.title, other.published_file_id),
                                    vec![other.dlc_name.clone()],
                                ))
                            } else if let Some(other) = library.compatibility.get(other_id) {
                                let intersect = other
                                    .ignore_required
                                    .intersection(&compat.ignore_required)
                                    .cloned()
                                    .collect::<Vec<_>>();
                                if intersect.is_empty() {
                                    None
                                } else {
                                    let conflict = cache
                                        .get_mod_metadata(other_id)
                                        .map(|other| {
                                            format!("{} ({})", other.title, other.published_file_id)
                                        })
                                        .unwrap_or_else(|| format!("UNKNOWN ({other_id})"));
                                    Some(CompatibilityIssue::Overlapping(conflict, intersect))
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }));
                }

                if let Some(ModDetails::Workshop(details)) = cache.get_details(id) {
                    issues.extend(details.children.iter().filter_map(|child| {
                        if let Ok(child_id) =
                            child.published_file_id.parse::<u32>().map(ModId::Workshop)
                            && !self.items.contains_key(&child_id)
                            && !metadata.iter().any(|entry| {
                                entry.key() == &child_id && provided.contains(&entry.dlc_name)
                            })
                        {
                            CompatibilityIssue::MissingWorkshop(
                                child_id.as_u32(),
                                format!(
                                    "{} ({})",
                                    child.published_file_id,
                                    cache
                                        .get_details(&child_id)
                                        .as_ref()
                                        .map(|details| details.title())
                                        .unwrap_or("UNKNOWN")
                                ),
                            )
                            .apply(Some)
                        } else {
                            None
                        }
                    }));
                }

                if !issues.is_empty() {
                    issues.sort();
                    all.insert(id.clone(), issues);
                }
            } else {
                all.insert(id.clone(), vec![CompatibilityIssue::Unknown]);
            }
        }

        self.compatibility_issues = all;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompatibilityIssue {
    /// Missing an item listed as a requirement by the Workshop page
    MissingWorkshop(u32, String),
    /// Missing an item listed as a requirement by their XComGame.ini
    MissingRequired(String),
    /// Conflicts with a mod listed by their XComGame.ini
    Incompatible(String),
    /// Provides the same DLCName(s)
    Overlapping(String, Vec<String>),
    /// The mod is missing its data
    Unknown,
}

pub mod profile_folder {
    pub const CHARACTER_POOL: &str = "CharacterPool";
    pub const CONFIG: &str = "Config";
    pub const PHOTOBOOTH: &str = "Photobooth";
    pub const SAVE_DATA: &str = "SaveData";

    pub const ALL: [&str; 4] = [CHARACTER_POOL, CONFIG, PHOTOBOOTH, SAVE_DATA];
}
