use std::{
    collections::{BTreeMap, VecDeque},
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::{ExitStatusError, Stdio},
    string::FromUtf8Error,
    sync::{Arc, atomic::AtomicBool},
};

use bstr::{BString, ByteSlice};
use iced::futures::{SinkExt, Stream, StreamExt, channel::mpsc::Sender};
use steam_rs::published_file_service::query_files::File;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines},
    process::ChildStdout,
    sync::{Mutex, broadcast},
};
use which::which;

use secrecy::{ExposeSecret, SecretString};

use crate::{
    Message, X2_WOTCCOMMUNITY_HIGHLANDER_ID, XCOM_APPID,
    files::{self, Cache},
    metadata::ProgramMetadata,
    steam_manifest::{AppWorkshopManifest, ManifestWorkshopItem, ManifestWorkshopItemDetails},
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
    #[error("SteamCMD session closed during operation")]
    SessionClosed,
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

pub type Result<R> = std::result::Result<R, Error>;

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
    Line(String),
    ReadError(String),
    WriteError(String),
}

#[derive(Debug, Clone)]
pub struct Session {
    buffer: Arc<Mutex<VecDeque<String>>>,
    request_sender: std::sync::mpsc::Sender<String>,
    event_receiver: Arc<broadcast::Receiver<SessionEvent>>,
    logged_in: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    killed: Arc<AtomicBool>,
    _child: Arc<std::process::Child>,
    _handles: Arc<[std::thread::JoinHandle<()>]>,
}

impl Session {
    /// Panics if run in async context (creates a new tokio runtime and blocks on it)
    pub fn init() -> Result<Session> {
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async { Self::init_async().await })
    }

    pub async fn init_async() -> Result<Session> {
        let mut command = steamcmd_command(&None)?;
        // Check errors manually
        command.args(["@ShutdownOnFailedCommand", "0"]);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());

        let mut child = command.spawn()?;
        let buffer = Arc::new(Mutex::new(VecDeque::new()));
        let killed = Arc::new(AtomicBool::new(false));

        let stdout = child.stdout.take().ok_or(Error::Message(
            "failed to steamcmd stdout handle".to_string(),
        ))?;
        let (event_sender, event_receiver) = broadcast::channel(16);
        let stdout_handle = std::thread::spawn({
            let event_sender = event_sender.clone();
            let reader = std::io::BufReader::new(stdout);
            let buffer = buffer.clone();
            let killed = killed.clone();
            move || {
                let mut lines = reader.lines();
                while let Some(Ok(line)) = lines.next() {
                    dbg!(&line);
                    let line = strip_ansi_escapes::strip_str(line);
                    buffer.blocking_lock().push_back(line.clone());
                    let _ = event_sender.send(SessionEvent::Line(line));
                }
                let _ = event_sender.send(SessionEvent::Shutdown);
                println!("SteamCMD output thread has shut down...");
                killed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });

        let mut stdin = child.stdin.take().ok_or(Error::Message(
            "failed to get steamcmd stdin handle".to_string(),
        ))?;
        let (request_sender, request_receiver) = std::sync::mpsc::channel::<String>();
        let stdin_handle = std::thread::spawn({
            let event_sender = event_sender.clone();
            move || {
                for line in request_receiver.iter() {
                    if let Err(err) = stdin.write_all(line.as_bytes()) {
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
            busy: Arc::new(AtomicBool::new(false)),
            killed,
            _child: Arc::new(child),
            _handles: Arc::new([stdout_handle, stdin_handle]),
        };

        // Potential TODO - Move this to its own function i.e. "boot"
        // that should be called immediately after successful init
        // Attempt to skip bootup
        println!("Attempting to skip bootup...");
        session.await_line().await.ok_or(Error::SessionClosed)?;
        while session.consume_line_async().await.is_some() {}
        println!("Session initialized");

        Ok(session)
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(std::sync::atomic::Ordering::Relaxed)
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
    pub fn lines(&self) -> impl Stream<Item = String> {
        iced::futures::stream::unfold(self.event_receiver.resubscribe(), |mut receiver| {
            Box::pin(async {
                loop {
                    if self.is_killed() {
                        return None;
                    }
                    match receiver.recv().await {
                        Ok(SessionEvent::Line(line)) => break Some((line, receiver)),
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

    /// Sends a command to the underlying SteamCMD session.
    /// Automatically appends a new line.
    fn send_command<S: Into<String>>(&self, command: S) -> Result<()> {
        self.request_sender.clone().send(command.into() + "\n")?;
        Ok(())
    }

    pub async fn find<S: AsRef<str>>(&self, s: S) -> Result<String> {
        self.send_command(format!(r#"find "{}""#, s.as_ref()))?;
        let mut result = String::new();
        while self.buffer_empty_async().await {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        while let Some(line) = self.consume_line_async().await {
            result.reserve_exact(line.len() + 1);
            result.push('\n');
            result.push_str(&line);
        }

        Ok(result)
    }

    pub async fn force_install_dir<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        if self.is_logged_in() {
            eprintln!("WARNING: Setting install directory while logged in is not recommended");
        }
        self.send_command(format!("force_install_dir {}", path.as_ref().display()))
    }

    pub async fn log_in_cached(&self, user: &str) -> Result<()> {
        self.send_command("@NoPromptForPassword 1")?;
        self.send_command(format!("login {user}"))?;
        while let Some(line) = self.await_line().await {
            if line.contains("FAILED (No cached credentials and @NoPromptForPassword is set)") {
                return Err(Error::LoginFailure);
            } else if line.contains("Waiting for client config...OK") {
                self.logged_in
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                return Ok(());
            } else {
                println!("Ignoring irrelevant line: {line}");
            }
        }
        Err(Error::Message("session closed".to_string()))
    }

    pub async fn log_in(&self, user: &str, pass: &str, code: &str) -> Result<bool> {
        self.send_command("@NoPromptForPassword 1")?;
        self.send_command(format!("login {user} {pass} {code}"))?;
        let mut lines = self.lines();
        while let Some(line) = lines.next().await {
            if line.contains("Waiting for client config...OK") {
                self.logged_in
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                return Ok(true);
            } else if line.contains("...ERROR (Invalid Password)") {
                return Err(Error::LoginFailure);
            }
        }
        Err(Error::Message("session closed".to_string()))
    }

    pub async fn log_out(&self) -> Result<()> {
        self.send_command("logout")?;
        while self.consume_line_async().await.is_some() {}
        Ok(())
    }

    pub async fn quit(&self) -> Result<()> {
        self.send_command("quit")?;
        self.await_line().await;
        self.killed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub async fn workshop_download_item(&self, app_id: u64, file_id: u64) -> Result<()> {
        self.send_command(format!("workshop_download_item {app_id} {file_id}"))?;
        let mut lines = self.lines();
        while let Some(line) = lines.next().await {
            if line.starts_with("ERROR!") {
                return Err(Error::Message(line));
            } else if line.starts_with("Success") {
                dbg!(line);
                return Ok(());
            }
        }
        Err(Error::SessionClosed)
    }
}

#[tokio::test]
async fn steamcmd_persistent() -> eyre::Result<()> {
    let session = Session::init_async().await?;
    println!("Getting commands");
    let commands = session.find("workshop").await?;
    println!("Commands:\n{commands}");

    println!("Setting install directory to /tmp");
    session.force_install_dir("/tmp/").await?;

    println!("Logging in");
    session.log_in_cached("anonymous").await?;

    println!("Downloading a mod that can be downloaded anonymously...");
    const BATTLE_ZONE_ID: u64 = 301650;
    const MOD_ID: u64 = 2829669454;
    session
        .workshop_download_item(BATTLE_ZONE_ID, MOD_ID)
        .await?;

    println!("Logging out");
    session.log_out().await?;
    println!("Quitting");
    session.quit().await?;

    drop(session);

    Ok(())
}

// TODO - Persistent SteamCMD session
// Currently all download requests require one login request per attempt,
// which can quickly lead to being rate limited if downloading lots of mods all at once.
// This can be alleviated by issuing on a rate-limit on our side,
// but we still only have a fraction of our allowed limit as long as
// login is enacted every single time.
#[derive(Debug, Default, Clone)]
pub struct State {
    pub username: String,
    pub password: SecretString,
    pub command_path: Option<PathBuf>,
    pub download_dir: PathBuf,
    pub logged_in: bool,
    pub is_cached: bool,
    pub running: bool,
    pub log_sender: Option<Sender<Message>>,
}

pub fn which_steamcmd(given: &Option<PathBuf>) -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let command = "steamcmd.exe";
    #[cfg(not(target_os = "windows"))]
    let command = "steamcmd";

    let path = given
        .to_owned()
        .or_else(|| which(command).ok())
        .ok_or(Error::MissingExecutable)?;

    if let Ok(true) = std::fs::exists(&path) {
        return Ok(path);
    }
    Err(Error::MissingExecutable)
}

/// Due to prominent issues with steamcmd in non-interactive sessions,
/// we try to unbuffer the output if possible (based on [this tip](https://github.com/ValveSoftware/Source-1-Games/issues/1929#issuecomment-1341353626)).
/// Relies on expect's `unbuffer` with coreutils `stdbuf` as a fallback.
/// Returns the standard command unaltered if neither are found on the system,
/// which may lead to degraded functionality.
///
/// Windows users will need to install uutils coreutils
/// and add `stdbuf` alias to their path for this to function.
fn steamcmd_command(given: &Option<PathBuf>) -> Result<std::process::Command> {
    #[cfg(target_os = "macos")]
    static STDBUF: &str = "gstdbuf";
    #[cfg(not(target_os = "macos"))]
    static STDBUF: &str = "stdbuf";

    let steamcmd = which_steamcmd(given)?;

    let command = if let Ok(unbuffer) = which("unbuffer") {
        let mut command = std::process::Command::new(unbuffer);
        command.arg(steamcmd);
        command
    } else if let Ok(stdbuf) = which(STDBUF) {
        let mut command = std::process::Command::new(stdbuf);
        command.arg("-o0").arg(steamcmd);
        command
    } else {
        std::process::Command::new(steamcmd)
    };

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let mut command = command;
        // Suppress console
        command.creation_flags(CREATE_NO_WINDOW);
        Ok(command)
    }
    #[cfg(not(target_os = "windows"))]
    Ok(command)
}

fn steamcmd_command_async(given: &Option<PathBuf>) -> Result<tokio::process::Command> {
    steamcmd_command(given).map(tokio::process::Command::from)
}

fn spawn_buffered(
    mut command: tokio::process::Command,
) -> Result<(tokio::process::Child, Lines<BufReader<ChildStdout>>)> {
    let mut process = command.stdout(std::process::Stdio::piped()).spawn()?;
    let stdout = process
        .stdout
        .take()
        .expect("stdout should be piped by default");
    let reader = tokio::io::BufReader::new(stdout);
    Ok((process, reader.lines()))
}

impl State {
    async fn send_log<B: Into<BString>>(&self, message: B) {
        if let Some(mut sender) = self.log_sender.clone()
            && let Err(err) = sender.send(Message::LogAppend(message.into())).await
        {
            eprintln!("Error sending log: {err:?}");
        }
    }

    async fn report_progress(&self, id: u32) {
        let Some(mut sender) = self.log_sender.clone() else {
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

    pub async fn attempt_cached_login(&self) -> Result<()> {
        let mut command = steamcmd_command_async(&self.command_path)?;
        command.args(["+@NoPromptForPassword", "1"]);
        command.args(["+login", self.username.as_str()]);
        command.arg("+quit");

        let (process, mut lines) = spawn_buffered(command)?;

        let captured = self.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                captured.send_log(line).await;
            }
        });

        let output = process.wait_with_output().await?;

        if output.status.success() {
            return Ok(());
        }

        Err(Error::LoginFailure)
    }

    pub async fn attempt_full_login(&self, guard_code: SecretString) -> Result<()> {
        let mut command = steamcmd_command_async(&self.command_path)?;
        command.args(["+@NoPromptForPassword", "1"]);
        command.args([
            "+login",
            self.username.as_str(),
            self.password.expose_secret(),
            guard_code.expose_secret(),
        ]);
        command.arg("+quit");

        let output = command.output().await?;
        self.send_log(output.stdout.clone()).await;

        if output.status.success() {
            return Ok(());
        }

        output.stdout.as_bstr().find(b"ERROR");

        Err(Error::LoginFailure)
    }

    // Note: synchronous since we only logout when exiting program.
    // Note that logout requires successful cached log in,
    // as the purpose is to clear the cached login details
    pub fn logout(&self) {
        if !(self.logged_in || self.is_cached) {
            return;
        }

        let result: eyre::Result<()> = try {
            let mut command = steamcmd_command(&self.command_path)?;
            command.args(["+@NoPromptForPassword", "1"]);
            command.args(["+login", &self.username]);
            command.arg("+logout");
            command.arg("+quit");

            command.status()?;
        };

        if let Err(err) = result {
            eprintln!("Error when trying to log out of steamcmd: {err:?}");
        }
    }

    pub async fn download_item(&self, id: u32) -> Result<u32> {
        let result: Result<u32> = try {
            std::fs::create_dir_all(&self.download_dir)?;

            let mut command = steamcmd_command_async(&self.command_path)?;
            command.args(["+@NoPromptForPassword", "1"]);
            command.arg("+force_install_dir");
            command.arg(&self.download_dir);
            command.args(["+login", &self.username]);
            command.args([
                "+workshop_download_item",
                &XCOM_APPID.to_string(),
                &id.to_string(),
                "validate",
            ]);
            command.arg("+quit");

            let buf_output = Arc::new(Mutex::new(Vec::new()));
            let (process, mut lines) = spawn_buffered(command)?;
            let captured = self.clone();
            let buf_clone = buf_output.clone();
            tokio::spawn(async move {
                while let Ok(Some(line)) = lines.next_line().await {
                    buf_clone.lock().await.push(line.clone());
                    captured.send_log(line).await;
                }
            });

            let captured = self.clone();
            let report_handle =
                tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        captured.report_progress(id).await;
                    }
                }));

            let output = process.wait_with_output().await?;

            if let Err(err) = output.exit_ok() {
                return Err(Error::DownloadFailure(
                    Box::new(Error::ProcessFailure(err)),
                    id,
                ));
            }

            drop(report_handle);

            // The download can fail while the process returns a non-error response
            if let Some(line) = buf_output
                .lock()
                .await
                .iter()
                .find(|line| line.contains(&format!("ERROR! Download item {id} failed")))
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

                return Err(Error::DownloadFailure(
                    Box::new(Error::Message(line.clone())),
                    id,
                ));
            }

            let path = files::get_item_directory(&self.download_dir, id);
            let last_updated = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("now should always be greater than unix epoch")
                .as_secs();
            let metadata = ProgramMetadata {
                time_downloaded: last_updated,
            };
            if let Err(err) = metadata.save_in(path) {
                eprintln!("Error writing LXCOMM metadata for {id}: {err:?}");
            }

            id
        };

        result.map_err(|err| Error::DownloadFailure(Box::new(err), id))
    }

    pub async fn download_item_with_retries(&self, id: u32, retries: u32) -> Result<u32> {
        let mut attempts = 0;
        loop {
            match self.download_item(id).await {
                ok @ Ok(_) => return ok,
                Err(_) if attempts < retries => attempts += 1,
                res => return res,
            }
        }
    }

    /// Note that due to very strange steamcmd behavior that does not appear to be addressed anywhere,
    /// by default, X2WOTCCommunityHighlander is downloaded before checking for updates because
    /// steamcmd will not check status if you don't (successfully) download something first.
    /// X2WOTCCommunityHighlander was chosen as the default since it's such a common dependency
    /// that it might as well come pre-installed.
    pub async fn check_updates(&self, ids: Vec<u32>, cache: Cache) -> Result<Vec<u32>> {
        let manifest_path = files::get_workshop_manifest_path(&self.download_dir);

        // Cancel if the user hasn't even downloaded any items yet
        match manifest_path.parent() {
            Some(parent) if !tokio::fs::try_exists(parent).await? => return Ok(Vec::new()),
            None => return Ok(Vec::new()),
            _ => (),
        }

        if tokio::fs::try_exists(&manifest_path).await? {
            tokio::fs::remove_file(&manifest_path).await?;
        }

        let mut manifest_file = std::fs::File::create_new(&manifest_path)?;
        let manifest = build_manifest(ids, cache);
        keyvalues_serde::to_writer_with_key(&mut manifest_file, &manifest, "AppWorkshop")
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;

        let needs_update = Vec::new();

        let mut command = steamcmd_command_async(&self.command_path)?;
        command.args(["+@NoPromptForPassword", "1"]);
        command.arg("+force_install_dir");
        command.arg(&self.download_dir);
        command.args(["+login", &self.username]);
        command.args([
            "+workshop_download_item",
            &XCOM_APPID.to_string(),
            &X2_WOTCCOMMUNITY_HIGHLANDER_ID.to_string(),
            "validate",
        ]);

        command.args(["+workshop_status", &XCOM_APPID.to_string()]);

        command.arg("+quit");

        let buf_output = Arc::new(Mutex::new(Vec::new()));
        let (process, mut lines) = spawn_buffered(command)?;
        let captured = self.clone();
        let buf_clone = buf_output.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                buf_clone.lock().await.push(line.clone());
                captured.send_log(line).await;
            }
        });

        let output = process.wait_with_output().await?;

        let output = buf_output.lock().await.join("\n");
        println!("Output: {output}");

        Ok(needs_update)
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
        .filter_map(|id| cache.get_details(id).map(|details| (id as u64, details)))
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
