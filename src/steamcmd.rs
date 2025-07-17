use std::{path::PathBuf, process::ExitStatusError, string::FromUtf8Error, sync::Arc};

use bstr::{BString, ByteSlice};
use iced::futures::{SinkExt, Stream, channel::mpsc::Sender};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines},
    process::ChildStdout,
    sync::Mutex,
};
use which::which;

use secrecy::{ExposeSecret, SecretString};

use crate::{Message, X2_WOTCCOMMUNITY_HIGHLANDER_ID, XCOM_APPID, files};

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
    let path = given
        .to_owned()
        .or_else(|| which("steamcmd").ok())
        .ok_or(Error::MissingExecutable)?;

    if let Ok(true) = std::fs::exists(&path) {
        return Ok(path);
    }
    Err(Error::MissingExecutable)
}

#[cfg(target_os = "linux")]
/// Due to prominent issues with steamcmd in non-interactive sessions,
/// we try to unbuffer the output if possible (based on [this tip](https://github.com/ValveSoftware/Source-1-Games/issues/1929#issuecomment-1341353626)).
/// Relies on expect's `unbuffer` with coreutils `stdbuf` as a fallback.
/// Returns the standard command unaltered if neither are found on the system,
/// which may lead to degraded functionality.
fn steamcmd_command(given: &Option<PathBuf>) -> Result<std::process::Command> {
    let steamcmd = which_steamcmd(given)?;

    if let Ok(unbuffer) = which("unbuffer") {
        let mut command = std::process::Command::new(unbuffer);
        command.arg(steamcmd);
        Ok(command)
    } else if let Ok(stdbuf) = which("stdbuf") {
        let mut command = std::process::Command::new(stdbuf);
        command.arg("-o0").arg(steamcmd);
        Ok(command)
    } else {
        Ok(std::process::Command::new(steamcmd))
    }
}

#[cfg(not(target_os = "linux"))]
fn steamcmd_command(given: &Option<PathBuf>) -> Result<std::process::Command> {
    Ok(std::process::Command::new(which_steamcmd(given)?))
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
    pub async fn check_updates(&self) -> Result<Vec<u32>> {
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
