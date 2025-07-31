use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
    io::BufRead,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::library::Library;

#[derive(Debug, Error)]
pub enum Error {
    #[error("missing field: {0}")]
    MissingField(&'static str),
    #[error("parse error: {0}")]
    ParseError(Box<dyn std::error::Error>),
    #[error("expected field {0}, got {1}")]
    MismatchedField(&'static str, String),
}
type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
/// The contents of `.XComMod` files.
pub struct ModMetadata {
    pub published_file_id: u32,
    pub title: String,
    pub description: String,
    pub requires_xpack: bool,
    /// The actual name of the mod being used
    pub dlc_name: String,
}

impl ModMetadata {
    pub fn deserialize_from_str<S: AsRef<str>, N: Into<String>>(
        source: S,
        name: N,
    ) -> Result<Self> {
        let lines = source
            .as_ref()
            .lines()
            .map(str::to_string)
            .collect::<Vec<String>>();
        let _header = lines
            .iter()
            .find(|line| line.contains("[mod]"))
            .ok_or(Error::MissingField("header"))?;

        macro_rules! find_line {
            ($label:expr,$type:ty) => {
                lines
                    .iter()
                    .filter_map(|line| line.split_once("="))
                    .find_map(|(left, right)| {
                        left.eq_ignore_ascii_case($label).then_some(
                            right
                                .parse::<$type>()
                                .map_err(|err| Error::ParseError(Box::new(err))),
                        )
                    })
                    .ok_or(Error::MissingField($label))
                    .flatten()
            };
        }

        let published_file_id = find_line!("publishedFileId", u32)?;
        let title = find_line!("Title", String)?;
        let description = find_line!("Description", String).unwrap_or_default();
        let requires_xpack = find_line!("RequiresXPACK", bool).unwrap_or_default();

        Ok(Self {
            published_file_id,
            title,
            description,
            requires_xpack,
            dlc_name: name.into(),
        })
    }
}

#[derive(Debug, Default)]
pub struct ModCompatibility {
    /// Mods that a mod should be loaded along with
    pub required: BTreeSet<String>,
    /// Mods that a mod cannot be loaded with
    pub incompatible: BTreeSet<String>,
    /// Additional mods that one can consider the mod as providing
    pub ignore_required: BTreeSet<String>,
}

impl ModCompatibility {
    pub fn extend_with(&mut self, other: Self) {
        self.required.extend(other.required);
        self.incompatible.extend(other.incompatible);
        self.ignore_required.extend(other.ignore_required);
    }
}

// Potential TODO - more robust parsing solution
pub fn parse_compatibility<R: std::io::Read>(reader: R) -> std::io::Result<ModCompatibility> {
    let mut required = BTreeSet::new();
    let mut incompatible = BTreeSet::new();
    let mut ignore_required = BTreeSet::new();

    for line in std::io::BufReader::new(reader).lines() {
        let line = line?;
        if line.starts_with("+IncompatibleMods=") {
            incompatible.insert(line["+IncompatibleMods=\"".len()..line.len() - 1].to_string());
        } else if line.starts_with("+RequiredMods=") {
            required.insert(line["+RequiredMods=\"".len()..line.len() - 1].to_string());
        } else if line.starts_with("+IgnoreRequiredMods=") {
            ignore_required
                .insert(line["+IgnoreRequiredMods=\"".len()..line.len() - 1].to_string());
        }
    }

    Ok(ModCompatibility {
        required,
        incompatible,
        ignore_required,
    })
}

pub fn scan_compatibility(folder: &Path) -> ModCompatibility {
    const MAX_SCAN_DEPTH: usize = 8;
    let mut compat = ModCompatibility::default();

    for entry in walkdir::WalkDir::new(folder)
        .max_depth(MAX_SCAN_DEPTH)
        .into_iter()
        .filter_entry(|d| d.file_name().eq_ignore_ascii_case("src"))
        .filter_map(|e| e.ok())
    {
        if entry.file_name() == "XComMod.ini"
            && let Ok(file) = std::fs::File::open(entry.path())
        {
            match parse_compatibility(file) {
                Ok(info) => compat.extend_with(info),
                Err(err) => {
                    eprintln!(
                        "Error attempting to parse compatbility info for file {}: {err:?}",
                        entry.path().display()
                    )
                }
            }
        }
    }

    compat
}

pub fn get_provided_mods<'a, I>(
    ids: I,
    metadata: &BTreeMap<ModId, ModMetadata>,
    library: &Library,
) -> BTreeSet<String>
where
    I: IntoIterator<Item = &'a ModId>,
{
    ids.into_iter()
        .flat_map(|id| {
            let mut provided = BTreeSet::new();
            if let Some(compat) = library.compatibility.get(id) {
                provided.extend(compat.ignore_required.iter());
            }
            if let Some(data) = metadata.get(id) {
                provided.insert(&data.dlc_name);
            }

            provided
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, strum::EnumIs)]
pub enum ModId {
    // Potential TODO - steam IDs can actually be up to 64 bits,
    // change all instances of workshop IDs to u64...
    Workshop(u32),
    Local(PathBuf),
}

impl ModId {
    pub fn maybe_workshop(&self) -> Option<u32> {
        match self {
            Self::Workshop(id) => Some(*id),
            Self::Local(_) => None,
        }
    }

    pub fn maybe_local(&self) -> Option<&Path> {
        match self {
            Self::Workshop(_) => None,
            Self::Local(path) => Some(path.as_path()),
        }
    }

    pub fn as_u32(&self) -> u32 {
        match self {
            Self::Local(_) => 0,
            Self::Workshop(id) => *id,
        }
    }

    /// Specifically for local mods in order to prevent name collisions
    pub fn get_hash(&self) -> String {
        match self {
            Self::Workshop(id) => id.to_string(),
            Self::Local(path) => {
                let hash = blake3::hash(path.display().to_string().as_bytes()).to_string();
                let name = path
                    .file_name()
                    .expect("local path should not be relative")
                    .display();
                format!("{name}_{hash}")
            }
        }
    }
}

impl From<&u32> for ModId {
    fn from(value: &u32) -> Self {
        Self::from(*value)
    }
}

impl From<u32> for ModId {
    fn from(value: u32) -> Self {
        Self::Workshop(value)
    }
}

impl From<PathBuf> for ModId {
    fn from(value: PathBuf) -> Self {
        Self::Local(value)
    }
}

impl Display for ModId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workshop(id) => f.write_str(&id.to_string()),
            Self::Local(path) => f.write_fmt(format_args!("{}", path.display())),
        }
    }
}

impl AsRef<ModId> for ModId {
    fn as_ref(&self) -> &ModId {
        self
    }
}
