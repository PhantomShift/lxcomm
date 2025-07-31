use std::{
    collections::{BTreeMap, VecDeque},
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::ExitStatusError,
    string::FromUtf8Error,
    sync::{Arc, atomic::AtomicBool},
};

use bstr::ByteSlice;
use derivative::Derivative;
use governor::DefaultDirectRateLimiter;
use iced::futures::{SinkExt, Stream, StreamExt, channel::mpsc::Sender};
use itertools::Itertools;
use steam_rs::published_file_service::query_files::File;
use thiserror::Error;
use tokio::sync::{Mutex, broadcast};
use which::which;

use secrecy::{ExposeSecret, SecretString};

use crate::{
    Message, XCOM_APPID,
    files::{self, Cache, ModDetails},
    metadata::{self, ProgramMetadata},
    steam_manifest::{AppWorkshopManifest, ManifestWorkshopItem, ManifestWorkshopItemDetails},
    xcom_mod::ModId,
};

/// Based on the documentation gathered by the LinuxGSM project.
/// https://docs.linuxgsm.com/steamcmd/errors
#[repr(usize)]
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    UNKNOWN_MAYBE_HLDS = 0x10E,
    INSUFFICIENT_SPACE1 = 0x202,
    UNKNOWN206 = 0x206,
    INSUFFICIENT_SPACE2 = 0x212,
    INTERNAL_CONNECTION_ISSUE = 0x402,
    UNKNOWN602 = 0x602,
    FILE_PERMISSION_ISSUE = 0x606,
    MISSING_UPDATE_FILES = 0x626,
    CORRUPT_UPDATE_FILES = 0x6A6,
    UNKNOWN2 = 0x2,
    CONNECTION_FAILURE = 0x6,

    RECONFIGURING = 0x3,
    VALIDATING = 0x5,
    PREALLOCATION = 0x11,
    DOWNLOADING = 0x61,
    COMMITTING = 0x101,
}

#[derive(Debug, Error, Default)]
pub enum Error {
    #[error("there is no underlying steamcmd session")]
    NoSession,
    #[error("an error occurred in the steamcmd session: {0}")]
    Session(#[from] SessionError),
    #[error("failed to send through channel: {0}")]
    SendError(#[from] std::sync::mpsc::SendError<String>),
    #[error("download failed")]
    DownloadFailure(Box<Error>, u32),
    #[error("child process exited with failure: {0}")]
    ProcessFailure(#[from] ExitStatusError),
    #[error("missing field: {0}")]
    MissingField(&'static str),
    #[error("failed to log in")]
    LoginFailure,
    #[error("failed to read steamcmd stdout")]
    ReadError(#[from] FromUtf8Error),
    #[error("io failure with steamcmd process")]
    IoError(#[from] std::io::Error),
    #[error("steamcmd could not be found")]
    MissingExecutable,
    #[error("{0}")]
    Message(String),
    #[default]
    #[error("unknown error occurred")]
    Unkown,
}

impl From<String> for Error {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum NeedsConfirmation {
    Yes,
    No,
}

#[derive(Debug, Default, Clone, Copy)]
pub enum SteamCMDExitCommand {
    #[default]
    Quit,
    LogoutAndQuit,
}

impl SteamCMDExitCommand {
    pub fn to_bytes(self) -> &'static [&'static [u8]] {
        match self {
            Self::Quit => &[b"quit"],
            Self::LogoutAndQuit => &[b"logout", b"quit"],
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum SessionEvent {
    /// Session was killed for some reason
    Shutdown,
    /// SteamCMD has output `Steam>` and is awaiting input
    Waiting,
    /// SteamCMD has outputted a line break
    Line(String),
    /// There was an error attempting to write to the session
    WriteError(String),
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("error initializing: {0}")]
    InitError(Box<Error>),
    #[error("error in pty session: {0}")]
    PtyError(#[from] anyhow::Error),
    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("session is busy")]
    Busy,
    #[error("session is killed")]
    Killed,
}

#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub struct Session {
    buffer: Arc<Mutex<VecDeque<String>>>,
    request_sender: std::sync::mpsc::Sender<String>,
    pub event_receiver: Arc<broadcast::Receiver<SessionEvent>>,
    logged_in: Arc<AtomicBool>,
    waiting: Arc<AtomicBool>,
    killed: Arc<AtomicBool>,
    _child: Arc<dyn portable_pty::Child + Send + Sync>,
    _handles: Arc<[std::thread::JoinHandle<()>]>,
    // Supposedly "some platforms" don't like
    // if this is dropped before the child process
    #[derivative(Debug = "ignore")]
    _master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
}

impl Session {
    pub fn init(steamcmd_path: Option<PathBuf>) -> Result<Session, SessionError> {
        let command = which_steamcmd(&steamcmd_path)
            .map_err(Box::new)
            .map_err(SessionError::InitError)?;

        let mut cmd = portable_pty::CommandBuilder::new(command);
        cmd.args(["@ShutdownOnFailedCommand", "0"]);

        let sys = portable_pty::native_pty_system();
        let portable_pty::PtyPair { master, slave } = sys.openpty(portable_pty::PtySize {
            rows: 80,
            cols: 256,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let child = slave.spawn_command(cmd)?;
        // Only using it for spawning a single session
        drop(slave);
        let mut writer = master.take_writer()?;
        let reader = master.try_clone_reader()?;

        let buffer = Arc::new(Mutex::new(VecDeque::new()));
        let killed = Arc::new(AtomicBool::new(false));
        let waiting = Arc::new(AtomicBool::new(false));

        let (event_sender, event_receiver) = broadcast::channel(16);
        let reader_handle = std::thread::spawn({
            let event_sender = event_sender.clone();
            let mut buf_reader = std::io::BufReader::new(reader);
            let buffer = buffer.clone();
            let killed = killed.clone();
            let waiting = waiting.clone();
            let mut scratch = Vec::with_capacity(1024);
            move || {
                while let Ok(read) = buf_reader.fill_buf() {
                    let count = read.len();
                    scratch.extend_from_slice(read);
                    buf_reader.consume(count);

                    // Tiny state machine because for some reason Valve
                    // sometimes decides to use ANSI escapes to move the terminal
                    // cursor to a new line instead of just... outputting a new line.
                    fn find_cursor_reposition(buffer: &[u8]) -> Option<usize> {
                        buffer
                            .as_bstr()
                            .find_iter(b"\x1b[")
                            .filter_map(|index| {
                                macro_rules! ensure {
                                    ($expression:expr) => {
                                        if !($expression) {
                                            return None;
                                        }
                                    };
                                }

                                let sub = &buffer[index + b"\x1b[".len()..];
                                let mut len = 0;
                                let mut bytes = sub.bytes().peekable();
                                ensure!(bytes.next()?.is_ascii_digit());
                                len += 1;
                                while bytes.next_if(u8::is_ascii_digit).is_some() {
                                    len += 1;
                                }
                                ensure!(bytes.next()? == b';');
                                len += 1;
                                ensure!(bytes.next()?.is_ascii_digit());
                                len += 1;
                                while bytes.next_if(u8::is_ascii_digit).is_some() {
                                    len += 1;
                                }
                                ensure!(bytes.next()? == b'H');
                                len += 1;

                                Some(index + len + 1)
                            })
                            .next()
                    }

                    // Potential TODO - Ensure these are found in correct order (search for all 3 at the same time)
                    while let Some(mut index) = scratch
                        .find_byteset(b"\r\n")
                        .or(find_cursor_reposition(&scratch))
                    {
                        let to_consume = index;
                        if scratch[index] == b'\r' && scratch.get(index + 1) == Some(&b'\n') {
                            index += 1;
                        }
                        let drain = scratch.drain(..=index);
                        // Lossy conversion because I don't feel like telling Steam what to do.
                        let mut line =
                            strip_ansi_escapes::strip(Vec::from_iter(drain.take(to_consume)))
                                .as_bstr()
                                .to_string();
                        // Skip command lines (redundancy + privacy reasons)
                        if !line.starts_with("Steam>") {
                            if line.starts_with("login") {
                                line.replace_range("login".len().., " [REDACTED]");
                            }
                            buffer.blocking_lock().push_back(line.clone());
                            let _ = event_sender.send(SessionEvent::Line(line));
                        }
                    }
                    if count > 0
                        && (scratch.ends_with_str("Steam>\x1b[0m")
                            || scratch.ends_with_str("Steam>\x1b[?25h")
                            || scratch.ends_with_str("Steam>"))
                    {
                        waiting.store(true, std::sync::atomic::Ordering::Relaxed);
                        let _ = event_sender.send(SessionEvent::Waiting);
                    } else {
                        waiting.store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                let _ = event_sender.send(SessionEvent::Shutdown);
                println!("SteamCMD output thread has shut down...");
                killed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });

        let (request_sender, request_receiver) = std::sync::mpsc::channel::<String>();
        let writer_handle = std::thread::spawn({
            let event_sender = event_sender.clone();
            move || {
                for line in request_receiver.iter() {
                    if let Err(err) = writer.write_all(line.as_bytes()) {
                        let _ = event_sender.send(SessionEvent::WriteError(err.to_string()));
                    }
                }
            }
        });

        let session = Self {
            buffer,
            request_sender,
            event_receiver: Arc::new(event_receiver),
            logged_in: Arc::new(AtomicBool::new(false)),
            waiting,
            killed,
            _child: Arc::from(child),
            _handles: Arc::new([reader_handle, writer_handle]),
            _master: Arc::new(Mutex::new(master)),
        };

        Ok(session)
    }

    pub async fn skip_boot(&self) {
        self.await_input().await;
        self.clear_buffer_async().await;
    }

    /// Returns whether or not the SteamCMD session is ready to receive input.
    pub fn is_waiting(&self) -> bool {
        self.waiting.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn is_logged_in(&self) -> bool {
        self.logged_in.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn is_killed(&self) -> bool {
        self.killed.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub async fn buffer_empty_async(&self) -> bool {
        self.buffer.lock().await.is_empty()
    }
    pub async fn clear_buffer_async(&self) {
        self.buffer.lock().await.clear();
    }
    pub async fn consume_line_async(&self) -> Option<String> {
        self.buffer.lock().await.pop_front()
    }
    /// Returns false if the session has shut down or some other error occurred.
    /// Returns true if SteamCMD is currently awaiting input at `Steam>`.
    pub async fn await_input(&self) -> bool {
        let mut receiver = self.event_receiver.resubscribe();
        loop {
            if self.is_killed() {
                break false;
            }
            if self.is_waiting() {
                break true;
            }
            match receiver.recv().await {
                Ok(SessionEvent::Waiting) => break true,
                Ok(SessionEvent::Shutdown) | Err(broadcast::error::RecvError::Closed) => {
                    break false;
                }
                _ => (),
            }
        }
    }
    pub async fn await_line(&self) -> Option<String> {
        let mut receiver = self.event_receiver.resubscribe();
        loop {
            if self.is_killed() {
                return None;
            }
            match receiver.recv().await {
                Ok(SessionEvent::Line(line)) => break Some(line),
                Ok(SessionEvent::Shutdown) => break None,
                Err(broadcast::error::RecvError::Closed) => break None,
                _ => {}
            }
        }
    }
    pub fn lines(&self) -> impl Stream<Item = String> + use<> {
        let rec = self.event_receiver.resubscribe();
        let killed = self.killed.clone();
        iced::futures::stream::unfold((rec, killed), |(mut receiver, killed)| {
            Box::pin(async {
                loop {
                    if killed.load(std::sync::atomic::Ordering::Relaxed) {
                        return None;
                    }
                    match receiver.recv().await {
                        Ok(SessionEvent::Line(line)) => break Some((line, (receiver, killed))),
                        Ok(SessionEvent::Shutdown) => break None,
                        Err(broadcast::error::RecvError::Closed) => break None,
                        _ => {}
                    }
                }
            })
        })
    }

    pub fn buffer_empty(&self) -> bool {
        self.buffer.blocking_lock().is_empty()
    }
    pub fn consume_line(&self) -> Option<String> {
        self.buffer.blocking_lock().pop_front()
    }
    pub fn consume_buffer(&self) -> Vec<String> {
        Vec::from_iter(self.buffer.blocking_lock().drain(..))
    }

    /// Sends a command to the underlying SteamCMD session.
    /// Automatically appends a new line.
    /// It is an error to send a command when it is known that the
    /// session is currently busy or is killed.
    pub fn send_command<S: Into<String>>(&self, command: S) -> Result<(), SessionError> {
        if !self.is_waiting() {
            return Err(SessionError::Busy);
        } else if self.is_killed() {
            return Err(SessionError::Killed);
        }

        self.request_sender
            .clone()
            .send(command.into() + "\n")
            .expect("session should still be alive");
        Ok(())
    }

    pub async fn find<S: AsRef<str>>(&self, s: S) -> std::result::Result<String, SessionError> {
        self.buffer_empty_async().await;
        self.send_command(format!(r#"find "{}""#, s.as_ref()))?;
        let mut result = String::new();
        self.await_input().await;
        while let Some(line) = self.consume_line_async().await {
            result.reserve_exact(line.len() + 1);
            result.push('\n');
            result.push_str(&line);
        }

        Ok(result)
    }

    pub async fn force_install_dir<P: AsRef<Path>>(&self, path: P) -> Result<(), SessionError> {
        if self.is_logged_in() {
            eprintln!("WARNING: Setting install directory while logged in is not recommended");
        }
        self.send_command(format!("force_install_dir {}", path.as_ref().display()))
    }

    pub async fn log_in_cached(&self, user: &str) -> Result<(), Error> {
        self.send_command("@NoPromptForPassword 1")?;
        self.send_command(format!("login {user}"))?;
        let mut lines = self.lines();
        while let Some(line) = lines.next().await {
            if line.contains("FAILED (No cached credentials and @NoPromptForPassword is set)") {
                self.await_input().await;
                return Err(Error::LoginFailure);
            } else if line.contains("Waiting for client config...OK") {
                self.await_input().await;
                self.logged_in
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                return Ok(());
            } else {
                eprintln!("Ignoring irrelevant line: {line}");
            }
        }
        Err(Error::Message("session closed".to_string()))
    }

    pub async fn log_in(&self, user: &str, pass: &str, code: &str) -> Result<(), Error> {
        self.send_command("@NoPromptForPassword 1")?;
        self.send_command(format!(r#"login "{user}" "{pass}" "{code}""#))?;
        let mut lines = self.lines();
        while let Some(line) = lines.next().await {
            if line.contains("Waiting for client config...OK") {
                self.await_input().await;
                self.logged_in
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                return Ok(());
            } else if line.contains("...ERROR (Invalid Password)") {
                return Err(Error::LoginFailure);
            }
        }
        Err(Error::Message("session closed".to_string()))
    }

    pub async fn log_out(&self) -> Result<(), SessionError> {
        self.send_command("logout")?;
        self.await_input().await;
        Ok(())
    }

    pub async fn quit(&self) -> Result<(), SessionError> {
        self.send_command("quit")?;
        self.await_line().await;
        self.killed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub async fn workshop_download_item(&self, app_id: u64, file_id: u64) -> Result<(), Error> {
        self.send_command(format!("workshop_download_item {app_id} {file_id}"))?;
        let mut lines = self.lines();
        while let Some(line) = lines.next().await {
            if line.starts_with("ERROR!") {
                return Err(Error::Message(line));
            } else if line.starts_with("Success") {
                return Ok(());
            }
        }
        if self.is_waiting() && !self.is_killed() {
            // There are probably a lot of error conditions that I haven't caught yet
            let buffer = self.buffer.lock().await.iter().join("\n");
            return Err(Error::Message(format!(
                "Some unhandled error occurred while downloading, dumping buffer:\n{buffer}"
            )));
        }
        Err(Error::Session(SessionError::Killed))
    }
}

#[tokio::test]
async fn steamcmd_persistent() -> eyre::Result<()> {
    let session = Session::init(None)?;

    let mut lines = session.lines();
    let _output_handle = tokio::spawn(async move {
        while let Some(line) = lines.next().await {
            println!("[SteamCMD] {line}");
        }
    });

    session.skip_boot().await;
    println!("[Test] Getting commands");
    let commands = session.find("workshop").await?;
    println!("[Test] Commands:\n{commands}");

    let temp_dir = std::env::temp_dir();
    println!("[Test] Setting install directory to {}", temp_dir.display());
    session.force_install_dir(temp_dir).await?;

    println!("[Test] Logging in");
    session.log_in_cached("anonymous").await?;

    println!("[Test] Downloading a mod that can be downloaded anonymously...");
    const BATTLE_ZONE_ID: u64 = 301650;
    const MOD_ID: u64 = 2829669454;
    session
        .workshop_download_item(BATTLE_ZONE_ID, MOD_ID)
        .await?;

    println!("[Test] Logging out");
    session.log_out().await?;
    println!("[Test] Quitting");
    session.quit().await?;

    drop(session);

    Ok(())
}

#[derive(Debug, Clone, Derivative)]
#[derivative(Default)]
pub struct State {
    pub username: String,
    pub password: SecretString,
    pub command_path: Option<PathBuf>,
    pub download_dir: PathBuf,
    pub is_cached: bool,
    pub session: Option<Session>,
    pub message_sender: Option<Sender<Message>>,
    #[derivative(Default(value = "Arc::new(download_ratelimiter())"))]
    pub rate_limit: Arc<DefaultDirectRateLimiter>,
}

fn download_ratelimiter() -> DefaultDirectRateLimiter {
    governor::DefaultDirectRateLimiter::direct(governor::Quota::per_minute(unsafe {
        std::num::NonZeroU32::new_unchecked(10)
    }))
}

pub fn which_steamcmd(given: &Option<PathBuf>) -> Result<PathBuf, Error> {
    #[cfg(target_os = "windows")]
    let command = "steamcmd.exe";
    #[cfg(not(target_os = "windows"))]
    let command = "steamcmd";

    let path = given
        .to_owned()
        .or_else(|| which(command).ok())
        .or_else(|| which("steamcmd.sh").ok())
        .ok_or(Error::MissingExecutable)?;

    if let Ok(true) = std::fs::exists(&path) {
        return Ok(path);
    }
    Err(Error::MissingExecutable)
}

impl State {
    fn try_get_session(&self) -> Result<&Session, Error> {
        self.session.as_ref().ok_or(Error::NoSession)
    }

    pub fn is_logged_in(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.is_logged_in())
    }

    async fn report_progress(&self, id: u32) {
        let Some(mut sender) = self.message_sender.clone() else {
            return;
        };

        let path = files::get_item_downloading_directory(&self.download_dir, id);
        if let Ok(size) = files::get_size(path)
            && let Err(err) = sender
                .send(Message::SteamCMDDownloadProgress(id, size))
                .await
        {
            eprintln!("Error reporting file size: {err:?}");
        }
    }

    pub async fn attempt_cached_login(&self) -> Result<(), Error> {
        let session = self.try_get_session()?;
        session.await_input().await;
        session.force_install_dir(&self.download_dir).await?;
        session.await_input().await;
        session.log_in_cached(&self.username).await
    }

    pub async fn attempt_full_login(&self, guard_code: SecretString) -> Result<(), Error> {
        let session = self.try_get_session()?;
        session.force_install_dir(&self.download_dir).await?;
        session.await_input().await;
        session
            .log_in(
                &self.username,
                self.password.expose_secret(),
                guard_code.expose_secret(),
            )
            .await
    }

    // Note: synchronous since we only logout when exiting program.
    // Note that logout requires successful cached log in,
    // as the purpose is to clear the cached login details
    pub fn logout(&self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if session.is_killed() || !session.is_logged_in() {
            return;
        }

        if let Some(Err(err)) = if let Ok(handle) = tokio::runtime::Handle::try_current() {
            Some(handle.block_on(async { session.log_out().await }))
        } else if let Ok(runtime) = tokio::runtime::Runtime::new() {
            Some(runtime.block_on(async { session.log_out().await }))
        } else {
            None
        } {
            eprintln!("Unhandled error while logging out: {err:?}");
        }
    }

    pub async fn download_item(&self, id: u32) -> Result<u32, Error> {
        let result: Result<u32, Error> = try {
            std::fs::create_dir_all(&self.download_dir)?;
            let session = self.try_get_session()?;
            session.force_install_dir(&self.download_dir).await?;

            let captured = self.clone();
            let report_handle =
                tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        captured.report_progress(id).await;
                    }
                }));

            self.rate_limit.until_ready().await;

            if let err @ Err(_) = session
                .workshop_download_item(XCOM_APPID as u64, id as u64)
                .await
            {
                // Try deleting the manifest file,
                // which is not strictly necessary for the app to function.
                match tokio::fs::remove_file(files::get_workshop_manifest_path(&self.download_dir))
                    .await
                {
                    Ok(()) => (),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => (),
                    Err(err) => {
                        eprintln!("Error attempting to remove manifest file: {err:?}");
                    }
                }

                err?
            };

            drop(report_handle);

            let path = files::get_item_directory(&self.download_dir, id);
            let time_downloaded = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("now should always be greater than unix epoch")
                .as_secs();
            let metadata = ProgramMetadata {
                time_downloaded,
                ..metadata::read_in(&path).unwrap_or_default()
            };
            if let Err(err) = metadata.save_in(path) {
                eprintln!("Error writing LXCOMM metadata for {id}: {err:?}");
            }

            id
        };

        result.map_err(|err| Error::DownloadFailure(Box::new(err), id))
    }

    pub async fn download_item_with_retries(&self, id: u32, retries: u32) -> Result<u32, Error> {
        let mut attempts = 0;
        loop {
            match self.download_item(id).await {
                ok @ Ok(_) => return ok,
                Err(_) if attempts < retries => attempts += 1,
                res => return res,
            }
        }
    }
}

pub fn setup_logging() -> impl Stream<Item = Message> {
    iced::stream::channel(100, async |mut output| {
        let (sender, mut receiver) = iced::futures::channel::mpsc::channel(100);
        if let Err(err) = output.send(Message::LoggingSetup(sender)).await {
            eprintln!("Error sending sender, logging for steamcmd will be unavailable");
            eprintln!("Error: {err:?}");
            return;
        }

        loop {
            use iced::futures::StreamExt;
            if let Err(err) = output.send(receiver.select_next_some().await).await {
                eprintln!("Error sending log: {err}");
            }
        }
    })
}

pub fn build_manifest(library_ids: Vec<u32>, cache: Cache) -> AppWorkshopManifest {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("now should always be after UNIX_EPOCH");

    let info = library_ids
        .into_iter()
        .filter_map(|id| {
            if let Some(ModDetails::Workshop(details)) = cache.get_details(ModId::Workshop(id)) {
                Some((id as u64, details))
            } else {
                None
            }
        })
        .collect::<BTreeMap<u64, Arc<File>>>();

    let size_on_disk: u64 = info
        .iter()
        .filter_map(|(_, details)| details.file_size.parse::<u64>().ok())
        .sum();

    let (workshop_items_installed, workshop_item_details): (
        BTreeMap<u64, ManifestWorkshopItem>,
        BTreeMap<u64, ManifestWorkshopItemDetails>,
    ) = info
        .iter()
        .filter_map(|(id, details)| {
            let manifest = details.hcontent_file.clone()?;
            let bytes_downloaded = details.file_size.parse::<u64>().ok()?;
            let latest_manifest = details.hcontent_file.clone()?;

            let item = ManifestWorkshopItem {
                size: bytes_downloaded,
                time_updated: 0,
                manifest: manifest.clone(),
            };

            let details = ManifestWorkshopItemDetails {
                manifest,
                time_updated: 0,
                time_touched: now.as_secs(),
                bytes_downloaded,
                bytes_to_download: 0,
                latest_time_updated: 0,
                latest_manifest,
            };

            Some(((*id, item), (*id, details)))
        })
        .unzip();

    AppWorkshopManifest {
        app_id: XCOM_APPID as u64,
        size_on_disk,
        needs_update: false,
        needs_download: false,
        time_last_updated: 0,
        time_last_app_ran: 0,
        last_build_id: 0,
        workshop_items_installed,
        workshop_item_details,
    }
}
