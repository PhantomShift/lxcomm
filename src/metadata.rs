use std::{collections::BTreeSet, fmt::Display, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::{files, web};

static DEFAULT_FILE_NAME: &str = ".lxcomm";

#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord, Hash)]
pub enum Tag {
    /// Official tags used by the workshop store page
    Workshop(web::XCOM2WorkshopTag),
    /// Tags that have been applied by the mod author
    Custom(Arc<str>),
    /// Tags that the user has defined themselves
    User(Arc<str>),
}

impl Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_ref())
    }
}

impl From<&str> for Tag {
    fn from(value: &str) -> Self {
        if let Some(tag) = web::XCOM2WorkshopTag::VARIANTS
            .iter()
            .find(|tag| tag.as_ref() == value)
        {
            return Self::Workshop(tag.to_owned());
        }

        Self::Custom(value.into())
    }
}

impl Serialize for Tag {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::User(tag) => serializer.serialize_str(&format!("USER:{tag}")),
            _ => serializer.serialize_str(self.as_ref()),
        }
    }
}

impl<'de> Deserialize<'de> for Tag {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let trimmed = s.trim_start_matches("USER:");
        if trimmed != s {
            Ok(Self::User(trimmed.into()))
        } else {
            Ok(Self::from(s.as_ref()))
        }
    }
}

impl AsRef<str> for Tag {
    fn as_ref(&self) -> &str {
        match self {
            Self::Workshop(tag) => tag.as_ref(),
            Self::Custom(tag) => tag.as_ref(),
            Self::User(tag) => tag.as_ref(),
        }
    }
}

#[derive(Debug, Clone, Hash, Serialize, Deserialize, Default)]
pub struct ProgramMetadata {
    pub time_downloaded: u64,
    pub tags: BTreeSet<Tag>,
}

impl ProgramMetadata {
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<(), eyre::Error> {
        let ser = toml::to_string(self)?;
        std::fs::write(path.as_ref(), ser)?;
        Ok(())
    }
    pub fn save_in<P: AsRef<Path>>(&self, path: P) -> Result<(), eyre::Error> {
        self.save(path.as_ref().join(DEFAULT_FILE_NAME))
    }
}

pub fn read_in<P: AsRef<Path>>(path: P) -> Option<ProgramMetadata> {
    let file = std::fs::read(path.as_ref().join(DEFAULT_FILE_NAME)).ok()?;
    toml::from_slice(&file).ok()
}

pub fn read_metadata<P: AsRef<Path>>(id: u32, download_dir: P) -> Option<ProgramMetadata> {
    read_in(files::get_item_directory(download_dir.as_ref(), id))
}
