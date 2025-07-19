use std::{
    fmt::Display,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

use iced::{
    Task,
    futures::{
        SinkExt, Stream, StreamExt,
        channel::mpsc::{Receiver, Sender},
    },
};
use itertools::{EitherOrBoth, Itertools};
use notify::{RecommendedWatcher, Watcher};
use steam_rs::published_file_service::query_files;

use crate::{DATA_DIR, Message, XCOM_APPID};

const KILO: u64 = 1024;
const MEGA: u64 = 1024 * 1024;
const GIGA: u64 = 1024 * 1024 * 1024;

static ITEM_FILE_DETAILS_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    let path = DATA_DIR.join("file_details");
    std::fs::create_dir_all(&path).expect("data directory should be writable");
    path
});

/// Errors if `from` is not a file.
/// Exists because although I don't plan on specifically
/// supporting Windows, for now I want to keep things generic where possible.
///
/// Note that this requires permissions to create symlinks,
/// which means that one of the following actions must be taken, in order of simplicity:
/// 1) Run LXCOMM with administrative privileges
/// 2) Run Windows in Developer Mode
/// 3) Add  `SeCreateSymbolicLink` permission to local user (requires gpedit.msc)
pub fn link_files<F: AsRef<Path>, T: AsRef<Path>>(from: F, to: T) -> Result<(), std::io::Error> {
    if from.as_ref().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "link_files should only be used for files",
        ));
    }

    #[cfg(not(target_os = "windows"))]
    std::os::unix::fs::symlink(from.as_ref(), to.as_ref())?;
    #[cfg(target_os = "windows")]
    std::os::windows::fs::symlink_file(from.as_ref(), to.as_ref())?;

    Ok(())
}

/// Note that this relies on the nightly feature [`junction_point`](https://doc.rust-lang.org/beta/std/os/windows/fs/fn.junction_point.html)
/// for functionality on Windows. Errors if `from` is not a directory.
pub fn link_dirs<F: AsRef<Path>, T: AsRef<Path>>(from: F, to: T) -> Result<(), std::io::Error> {
    if from.as_ref().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "link_dirs should only be used for directories",
        ));
    }

    #[cfg(not(target_os = "windows"))]
    std::os::unix::fs::symlink(from.as_ref(), to.as_ref())?;
    #[cfg(target_os = "windows")]
    std::os::windows::fs::junction_point(from.as_ref(), to.as_ref())?;

    Ok(())
}

pub fn get_all_items_directory<P: Into<PathBuf>>(download_dir: P) -> PathBuf {
    let mut path = download_dir.into();
    path.extend(["steamapps", "workshop", "content", &XCOM_APPID.to_string()]);
    path
}

pub fn get_item_directory<P: Into<PathBuf>>(download_dir: P, id: u32) -> PathBuf {
    get_all_items_directory(download_dir).join(id.to_string())
}

pub fn get_item_config_directory<P: Into<PathBuf>>(download_dir: P, id: u32) -> PathBuf {
    get_item_directory(download_dir, id).join("Config")
}

pub fn get_workshop_manifest_path<P: Into<PathBuf>>(download_dir: P) -> PathBuf {
    download_dir
        .into()
        .join(format!("steamapps/workshop/appworkshop_{XCOM_APPID}.acf"))
}

pub fn get_item_downloading_directory<P: Into<PathBuf>>(download_dir: P, id: u32) -> PathBuf {
    let mut path = download_dir.into();
    path.extend([
        "steamapps",
        "workshop",
        "downloads",
        &XCOM_APPID.to_string(),
        &id.to_string(),
    ]);
    path
}

pub fn get_size<P: AsRef<Path>>(path: P) -> Result<u64, std::io::Error> {
    let ty = std::fs::symlink_metadata(path.as_ref())?;
    if ty.is_file() {
        return Ok(ty.len());
    } else if ty.is_dir() {
        let mut size = 0;
        for dir in std::fs::read_dir(path.as_ref())? {
            size += get_size(dir?.path())?;
        }
        return Ok(size);
    }

    Ok(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SizeDisplay {
    Bytes(u64),
    Kibibytes(u64),
    Mebibytes(u64),
    Gibibytes(u64),
}

impl SizeDisplay {
    /// Automatically chooses the greatest unit that still
    /// displays as a whole number.
    pub fn automatic(size: u64) -> Self {
        match size {
            ..KILO => Self::Bytes(size),
            KILO..MEGA => Self::Kibibytes(size),
            MEGA..GIGA => Self::Mebibytes(size),
            GIGA.. => Self::Gibibytes(size),
        }
    }

    /// Automatically chooses the next greatest unit based on the given reference.
    pub fn automatic_from(size: u64, reference: u64) -> Self {
        match reference {
            ..KILO => Self::Bytes(size),
            KILO..MEGA => Self::Kibibytes(size),
            MEGA..GIGA => Self::Mebibytes(size),
            GIGA.. => Self::Gibibytes(size),
        }
    }

    pub fn as_float(&self) -> f64 {
        match self {
            Self::Bytes(size) => *size as f64,
            Self::Kibibytes(size) => *size as f64 / KILO as f64,
            Self::Mebibytes(size) => *size as f64 / MEGA as f64,
            Self::Gibibytes(size) => *size as f64 / GIGA as f64,
        }
    }

    pub fn suffix_short(&self) -> &'static str {
        match self {
            Self::Bytes(_) => "B",
            Self::Kibibytes(_) => "KiB",
            Self::Mebibytes(_) => "MiB",
            Self::Gibibytes(_) => "GiB",
        }
    }
}

impl Display for SizeDisplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let size = self.as_float();
        let decimals = f.precision().unwrap_or(2);
        let string = format!("{size:.decimals$}{}", self.suffix_short());
        f.pad_integral(decimals.cast_signed().signum() > -1, "", &string)
    }
}

#[derive(Debug, Clone)]
pub struct Cache {
    details_cache: moka::sync::Cache<u32, Arc<steam_rs::published_file_service::query_files::File>>,
}

impl Cache {
    /// Note that the insertion attempts to write to disk first,
    /// and will not insert if that operation fails first.
    pub fn insert_details(
        &mut self,
        id: u32,
        details: &query_files::File,
    ) -> Result<(), std::io::Error> {
        if !self.details_cache.contains_key(&id) {
            let path = ITEM_FILE_DETAILS_PATH.join(format!("{id}.json"));
            let file = std::fs::File::create(path)?;
            serde_json::to_writer(file, details)?;
            self.details_cache.insert(id, Arc::new(details.clone()));
        }

        Ok(())
    }

    pub fn get_details(&self, id: u32) -> Option<Arc<query_files::File>> {
        self.details_cache.optionally_get_with(id, || {
            let path = ITEM_FILE_DETAILS_PATH.join(format!("{id}.json"));
            if let Ok(json) = std::fs::read_to_string(path)
                && let Ok(file) = serde_json::from_str(&json)
            {
                Some(Arc::new(file))
            } else {
                None
            }
        })
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            details_cache: moka::sync::Cache::new(1024),
        }
    }
}

// Based on https://github.com/notify-rs/notify/blob/main/examples/async_monitor.rs
pub fn async_watcher()
-> notify::Result<(RecommendedWatcher, Receiver<notify::Result<notify::Event>>)> {
    let (mut tx, rx) = iced::futures::channel::mpsc::channel(100);

    let watcher = RecommendedWatcher::new(
        move |res| {
            iced::futures::executor::block_on(async {
                tx.send(res).await.unwrap();
            })
        },
        notify::Config::default(),
    )?;

    Ok((watcher, rx))
}

pub async fn watch_async<P: AsRef<Path>>(
    path: P,
    mut tx: Sender<notify::Result<notify::Event>>,
) -> notify::Result<()> {
    let (mut watcher, mut rx) = async_watcher()?;

    watcher.watch(path.as_ref(), notify::RecursiveMode::NonRecursive)?;

    while let Some(res) = rx.next().await {
        if let Err(err) = tx.send(res).await {
            eprintln!("Error sending file watch notification: {err:?}");
        }
    }

    Ok(())
}

pub fn setup_watching(path: PathBuf) -> impl Stream<Item = Message> {
    iced::stream::channel(100, async move |mut output| {
        let result: eyre::Result<()> = try {
            let (mut watcher, mut rx) = async_watcher()?;

            watcher.watch(&path, notify::RecursiveMode::NonRecursive)?;

            while let Some(res) = rx.next().await {
                match res {
                    Ok(event) => output.send(Message::FileWatchEvent(event)).await?,
                    Err(err) => eprintln!("Error occurred when watching files: {err:?}"),
                }
            }
        };

        if let Err(err) = result {
            eprintln!("Error occurred in file watching process: {err:?}");
        }
    })
}

// Something with rust's type resolver at the time of writing
// isn't allowing just `Subscription::run_with(p, setup_watching)`
// so this is manually implemented
pub struct WatcherRecipe(pub PathBuf);

impl iced::advanced::subscription::Recipe for WatcherRecipe {
    type Output = Message;

    // Hashed using the path itself
    fn hash(&self, state: &mut iced::advanced::subscription::Hasher) {
        use std::hash::Hash;
        self.0.display().to_string().hash(state);
    }

    fn stream(
        self: Box<Self>,
        _input: iced::advanced::subscription::EventStream,
    ) -> iced::advanced::graphics::futures::BoxStream<Self::Output> {
        Box::pin(setup_watching(self.0))
    }
}

/// Panics if `names` is an absolute path or is empty. Assumes case insensitivity.
pub fn find_directories_matching<P: Into<PathBuf>>(
    names: P,
) -> (Task<Option<PathBuf>>, iced::task::Handle) {
    let names = names.into();
    if names.has_root() || names.file_name().is_none() {
        panic!("names must have a length of at least 1");
    }

    Task::stream(iced::stream::channel(128, async move |mut sender| {
        let (async_send, mut async_rec) = tokio::sync::mpsc::channel(8);

        let walker = walkdir::WalkDir::new("/").min_depth(2);
        tokio::task::spawn_blocking(move || {
            for entry in walker
                .into_iter()
                .filter_entry(|p| {
                    !p.file_name()
                        .to_str()
                        .map(|s| s.starts_with("."))
                        .unwrap_or(false)
                })
                .filter_map(|entry| entry.ok())
            {
                let mut iter = entry.path().components();
                let components = iter.by_ref();
                components.find(|comp| {
                    comp.as_os_str().eq_ignore_ascii_case(
                        names
                            .components()
                            .next()
                            .expect("path should have at least one component"),
                    )
                });
                let matches = components
                    .zip_longest(names.components().skip(1))
                    .all(|pair| {
                        if let EitherOrBoth::Both(comp, name) = pair {
                            comp.as_os_str().eq_ignore_ascii_case(name)
                        } else {
                            false
                        }
                    });

                if matches
                    && let Err(err) = async_send.blocking_send(Some(entry.path().to_path_buf()))
                {
                    eprintln!("Error sending message from directory walking thread: {err:?}");
                    break;
                }
            }

            if let Err(err) = async_send.blocking_send(None) {
                eprintln!("Error sending message from directory walking thread: {err:?}");
            }
        });

        while let Some(message) = async_rec.recv().await {
            if let Err(err) = sender.send(message).await {
                eprintln!(
                    "Error sending message from task communicating for walking thread: {err:?}"
                );
            }
        }
    }))
    .abortable()
}
