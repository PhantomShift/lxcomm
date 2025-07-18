use std::{collections::BTreeMap, path::PathBuf, sync::LazyLock};

use fuse_rust::Fuse;
use serde::{Deserialize, Serialize};

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
    pub filter_fuzzy: bool,
    pub filter_query: String,
}

impl Library {
    pub fn iter_selected(&self) -> impl Iterator<Item = &LibraryItem> {
        self.inner.values().filter(|item| item.selected)
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
        self.inner
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
        self.inner
            .values_mut()
            .filter(move |item| filter_method.matches(item))
    }

    pub fn iter_filtered_mut(&mut self) -> impl Iterator<Item = &mut LibraryItem> {
        self.iter_with_filter_mut(self.get_filter_method())
    }

    pub fn update_selected_filtered(&mut self) {
        let method = self.get_filter_method();
        for item in self.inner.values_mut() {
            item.selected = item.selected && method.matches(item);
        }
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
