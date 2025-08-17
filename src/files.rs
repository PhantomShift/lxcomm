use std::{
    fmt::Display,
    io::{BufReader, Read, Seek},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

use bstr::ByteSlice;
use iced::{
    Task,
    futures::{
        SinkExt, Stream, StreamExt,
        channel::mpsc::{Receiver, Sender},
    },
};
use itertools::{EitherOrBoth, Itertools};
use notify::Watcher;
use steam_rs::published_file_service::query_files;

use moka::sync::Cache as MokaCache;

#[cfg(target_os = "windows")]
use notify::PollWatcher as PlatformWatcher;
#[cfg(not(target_os = "windows"))]
use notify::RecommendedWatcher as PlatformWatcher;

use crate::{
    DATA_DIR, Message, XCOM_APPID, metadata,
    xcom_mod::{self, ModId},
};

pub const KILO: u64 = 1024;
pub const MEGA: u64 = 1024 * 1024;
pub const GIGA: u64 = 1024 * 1024 * 1024;

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

/// First attempts to rename F to T, falling back to directly copying if
/// F and T are on different mount points and deleting F.
/// Additionally errors if F is not a directory or T exists and is not a directory.
pub fn move_dirs<F: AsRef<Path>, T: AsRef<Path>>(from: F, to: T) -> Result<(), std::io::Error> {
    if !from.as_ref().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "from must be a directory",
        ));
    }
    if to.as_ref().try_exists()? && !to.as_ref().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "to must be a directory",
        ));
    }

    if let Err(err) = std::fs::rename(from.as_ref(), to.as_ref()) {
        match err.kind() {
            std::io::ErrorKind::CrossesDevices => {
                dircpy::copy_dir(from.as_ref(), to.as_ref())?;
                std::fs::remove_dir_all(from)?;
            }
            _ => return Err(err),
        }
    }

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

pub fn get_mod_directory<P: Into<PathBuf>>(download_dir: P, id: &ModId) -> PathBuf {
    match id {
        ModId::Workshop(id) => get_item_directory(download_dir, *id),
        ModId::Local(path) => path.clone(),
    }
}

pub fn get_mod_config_directory<P: Into<PathBuf>>(download_dir: P, id: &ModId) -> PathBuf {
    get_mod_directory(download_dir, id).join("Config")
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

#[derive(Clone)]
pub struct Cache {
    details_cache: MokaCache<u32, Arc<query_files::File>>,
    program_metadata_cache: MokaCache<PathBuf, metadata::ProgramMetadata>,
    mod_metadata_cache: Arc<dashmap::DashMap<ModId, xcom_mod::ModMetadata>>,
}

#[derive(Debug, Clone)]
pub enum ModDetails {
    Workshop(Arc<query_files::File>),
    Local(xcom_mod::ModMetadata),
}

impl ModDetails {
    pub fn title(&self) -> &str {
        match self {
            Self::Workshop(details) => &details.title,
            Self::Local(details) => &details.title,
        }
    }

    pub fn children(&self) -> &[query_files::ChildFile] {
        match self {
            Self::Workshop(details) => details.children.as_slice(),
            Self::Local(_) => &[],
        }
    }

    // TODO - Finish implementing tags (this should not exist when feature is done)
    pub fn tags(&self) -> &[query_files::Tag] {
        match self {
            Self::Workshop(details) => details.tags.as_slice(),
            Self::Local(_) => &[],
        }
    }

    pub fn maybe_workshop(self) -> Option<Arc<query_files::File>> {
        match self {
            Self::Workshop(details) => Some(details),
            Self::Local(_) => None,
        }
    }
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

    pub fn get_details<I: AsRef<ModId>>(&self, id: I) -> Option<ModDetails> {
        match id.as_ref() {
            ModId::Workshop(id) => self
                .details_cache
                .optionally_get_with(*id, || {
                    let path = ITEM_FILE_DETAILS_PATH.join(format!("{id}.json"));
                    if let Ok(json) = std::fs::read_to_string(path)
                        && let Ok(file) = serde_json::from_str(&json)
                    {
                        Some(Arc::new(file))
                    } else {
                        None
                    }
                })
                .map(ModDetails::Workshop),
            id @ ModId::Local(_) => self
                .mod_metadata_cache
                .get(id)
                .map(|entry| entry.to_owned())
                .map(ModDetails::Local),
        }
    }

    pub fn invalidate_details(&self, id: &ModId) {
        match id {
            ModId::Workshop(id) => self.details_cache.invalidate(id),
            ModId::Local(_) => {
                self.mod_metadata_cache.remove(id);
            }
        }
    }

    pub fn update_mod_metadata(&self, id: ModId, data: xcom_mod::ModMetadata) {
        self.mod_metadata_cache.insert(id, data);
    }

    pub fn get_mod_metadata(
        &'_ self,
        id: &ModId,
    ) -> Option<dashmap::mapref::one::Ref<'_, ModId, xcom_mod::ModMetadata>> {
        self.mod_metadata_cache.get(id)
    }

    pub fn update_metadata<P: AsRef<Path>>(
        &self,
        path: P,
        data: metadata::ProgramMetadata,
    ) -> Result<(), eyre::Error> {
        data.save_in(path.as_ref())?;
        self.program_metadata_cache
            .insert(path.as_ref().to_path_buf(), data);
        Ok(())
    }

    pub fn get_metadata<P: AsRef<Path>>(&self, path: P) -> metadata::ProgramMetadata {
        self.program_metadata_cache
            .get_with_by_ref(path.as_ref(), || {
                metadata::read_in(path.as_ref()).unwrap_or_default()
            })
    }

    pub fn get_mod_metadata_list(&self) -> &dashmap::DashMap<ModId, xcom_mod::ModMetadata> {
        &self.mod_metadata_cache
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            details_cache: MokaCache::new(1024),
            program_metadata_cache: MokaCache::new(1024),
            mod_metadata_cache: Arc::new(dashmap::DashMap::new()),
        }
    }
}

// Based on https://github.com/notify-rs/notify/blob/main/examples/async_monitor.rs
pub fn async_watcher() -> notify::Result<(PlatformWatcher, Receiver<notify::Result<notify::Event>>)>
{
    let (mut tx, rx) = iced::futures::channel::mpsc::channel(100);
    let watcher = PlatformWatcher::new(
        move |res| {
            iced::futures::executor::block_on(async {
                tx.send(res).await.unwrap();
            })
        },
        notify::Config::default().with_poll_interval(std::time::Duration::from_millis(200)),
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

#[derive(Debug, Clone)]
pub enum MonitorFileChange {
    Create(String),
    Append(String),
}

pub fn monitor_file_changes(path: PathBuf) -> (Task<MonitorFileChange>, iced::task::Handle) {
    Task::stream(iced::stream::channel(16, async move |mut output| {
        let res: eyre::Result<()> = try {
            let (mut watcher, mut receiver) = async_watcher()?;
            watcher.watch(&path, notify::RecursiveMode::NonRecursive)?;

            let mut position = 0;
            while let Some(event) = receiver.next().await {
                match event {
                    Err(err) => {
                        eprintln!("notify-rs event error: {err:?}");
                    }
                    Ok(event) => match event {
                        notify::Event {
                            kind: notify::EventKind::Create(_),
                            ..
                        } => {
                            let (len, contents) = if let Ok(bytes) = tokio::fs::read(&path).await {
                                (bytes.len(), bytes.to_str_lossy().to_string())
                            } else {
                                (0, String::new())
                            };
                            if output
                                .send(MonitorFileChange::Create(contents))
                                .await
                                .is_ok()
                            {
                                position += len;
                            }
                        }
                        notify::Event {
                            kind:
                                notify::EventKind::Modify(
                                    notify::event::ModifyKind::Data(
                                        notify::event::DataChange::Content
                                        | notify::event::DataChange::Size
                                        | notify::event::DataChange::Any,
                                    )
                                    | notify::event::ModifyKind::Metadata(
                                        notify::event::MetadataKind::WriteTime,
                                    ),
                                ),
                            ..
                        } => {
                            let path = path.clone();
                            let (read, contents) =
                                tokio::task::spawn_blocking(move || -> eyre::Result<_> {
                                    let file = std::fs::File::open(path)?;
                                    let mut buffer = Vec::new();
                                    let mut buf_reader = BufReader::new(file);
                                    buf_reader.seek(std::io::SeekFrom::Start(position as u64))?;
                                    let read = buf_reader.read_to_end(&mut buffer)?;
                                    Ok((read, buffer.to_str_lossy().to_string()))
                                })
                                .await
                                .ok()
                                .and_then(Result::ok)
                                .unwrap_or_default();
                            let message = if position == 0 {
                                MonitorFileChange::Create(contents)
                            } else {
                                MonitorFileChange::Append(contents)
                            };
                            if output.send(message).await.is_ok() {
                                position += read;
                            }
                        }
                        _ => (),
                    },
                }
            }
        };

        if let Err(err) = res {
            eprintln!("Error when operating file watcher: {err:?}");
        }
    }))
    .abortable()
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

pub fn gen_hash(source: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"LXCOMM");
    hasher.update(source);
    hasher.finalize().to_hex().to_string()
}

pub fn gen_partial_hash(source: &Path, up_to: usize) -> std::io::Result<String> {
    let file = std::fs::File::open(source)?;
    let mut buf_reader = BufReader::new(file);
    let mut buf = vec![0; up_to];
    let _ = buf_reader.read_exact(&mut buf);
    Ok(gen_hash(&buf))
}

/// Checks if two files have the filename and contents.
pub fn file_exact_eq(a: &Path, b: &Path) -> std::io::Result<bool> {
    if a.file_name() != b.file_name() {
        return Ok(false);
    }

    let meta_a = a.metadata()?;
    let meta_b = b.metadata()?;

    if meta_a.len() != meta_b.len() {
        return Ok(false);
    }

    if meta_a.is_file() != meta_b.is_file() {
        return Ok(false);
    }

    let [hash_a, hash_b] = std::thread::scope(|s| {
        let [a, b] = [a, b].map(|path| {
            s.spawn(move || -> std::io::Result<blake3::Hash> {
                let mut hasher = blake3::Hasher::new();
                hasher.update_reader(BufReader::new(std::fs::File::open(path)?))?;
                Ok(hasher.finalize())
            })
        });
        [
            a.join().expect("thread should not panic"),
            b.join().expect("thread should not panic"),
        ]
    });

    Ok(hash_a? == hash_b?)
}

pub fn dir_exact_eq(a: &Path, b: &Path) -> std::io::Result<bool> {
    macro_rules! ensure {
        ($expression:expr) => {
            if !($expression) {
                return Ok(false);
            }
        };
    }

    let a = a.canonicalize()?;
    let b = b.canonicalize()?;

    // Theoretically if file structures are identical then walks
    // should have identical paths.
    let mut walk_a = walkdir::WalkDir::new(&a)
        .sort_by_file_name()
        .into_iter()
        // Skip checking name of root directory.
        .skip(1);
    let mut walk_b = walkdir::WalkDir::new(&b)
        .sort_by_file_name()
        .into_iter()
        .skip(1);

    for (entry_a, entry_b) in walk_a.by_ref().zip(walk_b.by_ref()) {
        let entry_a = entry_a?;
        let entry_b = entry_b?;

        ensure!(entry_a.depth() == entry_b.depth());
        ensure!(entry_a.file_name() == entry_b.file_name());

        let rel_a = entry_a
            .path()
            .strip_prefix(&a)
            .expect("subpath should be prefixed with its root");
        let rel_b = entry_b
            .path()
            .strip_prefix(&b)
            .expect("subpath should be prefixed with its root");
        ensure!(rel_a == rel_b);

        match (entry_a.metadata()?.is_file(), entry_b.metadata()?.is_file()) {
            (true, true) if file_exact_eq(entry_a.path(), entry_b.path())? => (),
            (false, false) => (),
            _ => {
                dbg!(entry_a.path());
                dbg!(entry_b.path());
                return Ok(false);
            }
        }
    }

    ensure!(walk_a.next().is_none() && walk_b.next().is_none());

    Ok(true)
}
