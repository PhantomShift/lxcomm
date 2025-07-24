use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::files;

static DEFAULT_FILE_NAME: &str = ".lxcomm";

#[derive(Debug, Clone, Hash, Serialize, Deserialize)]
pub struct ProgramMetadata {
    pub time_downloaded: u64,
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
