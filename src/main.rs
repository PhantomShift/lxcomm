#![feature(try_blocks)]
#![feature(iter_intersperse)]
#![feature(exit_status_error)]
#![feature(path_add_extension)]
#![feature(iter_map_windows)]
// For windows soft links
#![cfg_attr(target_os = "windows", feature(junction_point))]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    hash::Hash,
    ops::Not,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, LazyLock},
    time::Duration,
};

use apply::Apply;
use bevy_reflect::{GetField, Reflect, StructInfo, Type, TypeInfo, Typed};
use bstr::{BString, ByteSlice};
use derivative::Derivative;
use etcetera::{AppStrategy, AppStrategyArgs};
use eyre::Result;
use iced::{
    Alignment::Center,
    Element,
    Length::{self, Fill, Shrink},
    Subscription, Task, Theme,
    alignment::Vertical,
    futures::{
        SinkExt, StreamExt,
        channel::mpsc::{Receiver, Sender},
    },
    stream,
    widget::{
        self, Stack, button, checkbox, column, combo_box, container, horizontal_space, image,
        markdown, opaque, pick_list, progress_bar, rich_text, row, scrollable, span, stack, text,
        text_editor, text_input, toggler, tooltip, vertical_rule, vertical_space,
    },
};
use iced_aw::{card, widget::LabeledFrame};
use itertools::Itertools;
use ringmap::RingMap;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use steam_rs::{self, Steam, published_file_service::query_files::PublishedFiles};
use strum::{Display, EnumIter, IntoEnumIterator, VariantArray};

use crate::mod_edit::EditorMessage;
use crate::platform::symbols;
use crate::{extensions::DetailsExtension, web::resolve_all_dependencies};

pub mod extensions;
pub mod files;
pub mod library;
pub mod loading;
pub mod markup;
pub mod mod_edit;
pub mod platform;
pub mod steam_manifest;
pub mod steamcmd;
pub mod web;
pub mod widgets;
pub mod xcom_mod;

const XCOM_APPID: u32 = 268500;
const X2_WOTCCOMMUNITY_HIGHLANDER_ID: u32 = 1134256495;

static APP_STRATEGY_ARGS: LazyLock<AppStrategyArgs> = LazyLock::new(|| AppStrategyArgs {
    author: "phantomshift".to_string(),
    top_level_domain: "io.github".to_string(),
    app_name: "lxcomm".to_string(),
});

#[cfg(target_os = "linux")]
static APP_SESSION_NAME: &str = "io.github.phantomshift.lxcomm";
#[cfg(target_os = "linux")]
static APP_SESSION_PATH: &str = "/io/github/phantomshift/lxcomm";

fn get_strategy() -> impl AppStrategy {
    etcetera::app_strategy::choose_native_strategy(APP_STRATEGY_ARGS.clone())
        .expect("home directory should be findable in system")
}
static DATA_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = get_strategy().data_dir();
    std::fs::create_dir_all(&dir).expect("data directory should be writable");
    dir
});
static CACHE_DIR: LazyLock<PathBuf> = LazyLock::new(|| get_strategy().cache_dir());
static CONFIG_DIR: LazyLock<PathBuf> = LazyLock::new(|| get_strategy().config_dir());
static CACHED_QUERIES: LazyLock<PathBuf> = LazyLock::new(|| {
    let cache = CACHE_DIR.join("queries");
    if !cache.is_dir() {
        std::fs::create_dir_all(&cache).expect("cache directory should be writable")
    }
    cache
});

static SETTINGS_PATH: LazyLock<PathBuf> = LazyLock::new(|| CONFIG_DIR.join("settings.json"));
static SAVE_PATH: LazyLock<PathBuf> = LazyLock::new(|| DATA_DIR.join("data.json"));

static PROFILES_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let profiles = DATA_DIR.join("profiles");
    std::fs::create_dir_all(&profiles).expect("data directory should be writable");
    profiles
});

static ACTIVE_CONFIG_DIR: LazyLock<PathBuf> = LazyLock::new(|| DATA_DIR.join("active_config"));
static ACTIVE_MODS_DIR: LazyLock<PathBuf> = LazyLock::new(|| DATA_DIR.join("active_mods"));

#[cfg(target_os = "linux")]
static NOTIF_CACHE: LazyLock<moka::sync::Cache<u32, u32>> = LazyLock::new(|| {
    moka::sync::Cache::builder()
        .time_to_idle(Duration::from_secs(10))
        .max_capacity(16)
        .build()
});

#[derive(Debug, Default, Clone)]
pub enum SettingsMessage {
    Edit(AppSettingEdit),
    ResetToDefault,
    ResetToSaved,
    Save,
    #[default]
    None,
}

impl From<AppSettingEdit> for SettingsMessage {
    fn from(value: AppSettingEdit) -> Self {
        Self::Edit(value)
    }
}

#[derive(Debug, Default, Clone)]
pub enum Message {
    Chained(Vec<Message>),

    QueryFilesLoaded(PublishedFiles),

    AsyncDialogResolve(AsyncDialogKey, usize),
    AsyncDialogCancelled(AsyncDialogKey),
    AbortTask(AppAbortKey),

    ApiKeyRequest,
    ApiKeyRequestUpdate(String),
    ApiKeySubmit,

    SetApiKey(String),
    SetBrowsePage(u32),
    SetViewingItem(u32),
    BrowseEditQuery(String),
    BrowseEditTag(web::XCOM2WorkshopTag),
    BrowseEditSort(web::WorkshopSort),
    BrowseEditPeriod(web::WorkshopTrendPeriod),
    BrowseToggleTagsDropdown(bool),
    BrowseSubmitQuery,
    BrowseUpdateQuery,

    SetSteamCMDUser(String),
    SetSteamCMDPassword(String),
    SetSteamCMDCode(String),
    SteamCMDLogin(bool),
    SteamCMDLoginSuccess,
    SteamCMDLoginElevated,
    SteamCMDLoginElevatedSubmit,
    SteamCMDLoginElevatedCancel,

    DeleteRequested(u32),
    SteamCMDDownloadRequested(u32),
    SteamCMDDownloadCompleted(u32),
    SteamCMDDownloadErrored(u32, String),
    SteamCMDDownloadProgress(u32, u64),
    SteamCMDDownloadCompletedClear(Vec<u32>),
    SteamCMDDownloadErrorClear(Vec<u32>),
    DownloadCancelRequested(u32),
    SteamCMDCheckUpdateRequested,
    SteamCMDCheckUpdateCompleted(Vec<u32>),
    DownloadAllRequested(u32),
    DownloadAllConfirmed(Arc<BTreeSet<u32>>),

    Settings(SettingsMessage),
    ModEditor(EditorMessage),

    LibraryToggleItem(u32, bool),
    LibraryToggleAll(bool),
    LibraryUpdateRequest,
    LibraryScanRequest,
    LibraryAddToProfileRequest,
    LibraryAddToProfileToggleAll(bool),
    LibraryAddToProfileToggled(usize, bool),
    LibraryAddToProfileConfirm(Vec<u32>),
    LibraryDeleteRequest,
    LibraryDeleteConfirm,
    LibraryFilterUpdateQuery(String),
    LibraryFilterToggleFuzzy(bool),
    ProfileAddPressed,
    ProfileAddEdited(String),
    ProfileAddCompleted(bool),
    ProfileAddItems(usize, Vec<u32>),
    ProfileDeletePressed(usize),
    ProfileRemoveItems(usize, Vec<u32>),
    ProfileSelected(usize),
    ProfileItemSelected(u32),
    ProfileImportSaveRequested(usize),
    ProfileViewSaveRequested(usize),
    ActiveProfileSelected(String),

    LoadPrepareProfile(bool),
    LoadPickGameDirectory,
    LoadSetGameDirectory(PathBuf),
    LoadPickLocalDirectory,
    LoadSetLocalDirectory(PathBuf),
    LoadPickLaunchCommand,
    LoadSetLaunchCommand(String),
    LoadAddLaunchArgs,
    LoadRemoveLaunchArgs(usize),
    LoadEditArgKey(usize, String),
    LoadEditArgValue(usize, String),
    LoadLaunchGame,

    LoadFindGameRequested,
    LoadFindGameMatched(PathBuf),
    LoadFindGameResolved(PathBuf),
    LoadFindLocalRequested,
    LoadFindLocalMatched(PathBuf),
    LoadFindLocalResolved(PathBuf),

    ItemDetailsAddToLibraryRequest(Vec<u32>),
    ItemDetailsAddToLibraryAllRequest(u32),

    SetPage(AppPage),
    SetBusy(bool),
    SetBusyMessage(String),
    DisplayError(String, String),
    OpenModal(Arc<AppModal>),
    CloseModal,
    EscapeModal,

    // Subscription-related
    #[cfg(target_os = "linux")]
    DbusError(zbus::Error),

    GainFocus,
    CloseAppRequested,
    LoggingSetup(Sender<Message>),
    LogAppend(BString),
    LogAction(iced::widget::text_editor::Action),
    FileWatchEvent(notify::Event),
    BackgroundResolverMessage(web::ResolverMessage),
    LaunchLogAction(iced::widget::text_editor::Action),
    LaunchLogCreated(String),
    LaunchLogAppended(String),
    LaunchLogClear,

    // Web-related
    ImageLoaded(String, image::Handle),

    #[default]
    None,
}

impl Message {
    fn display_error<T: Into<String>, M: Into<String>>(title: T, message: M) -> Self {
        Self::DisplayError(title.into(), message.into())
    }
}

impl From<SettingsMessage> for Message {
    fn from(value: SettingsMessage) -> Self {
        Self::Settings(value)
    }
}

impl From<AppSettingEdit> for Message {
    fn from(value: AppSettingEdit) -> Self {
        Self::Settings(SettingsMessage::from(value))
    }
}

#[derive(Debug, Default, Clone, Copy, EnumIter, Display, PartialEq, Eq)]
pub enum AppPage {
    #[default]
    Main,
    Library,
    Profiles,
    Browse,
    SteamCMD,
    Settings,
    Downloads,
    #[strum(to_string = "Game Logs")]
    GameLogs,
}

#[derive(Debug, Derivative, Clone)]
#[derivative(PartialEq)]
pub enum AppModal {
    ApiKeyRequest,
    AddToProfileRequest(Vec<u32>),
    LibraryDeleteRequest,
    SteamGuardCodeRequest,
    ProfileAddRequest,
    ItemDetailedView(u32),
    Busy,
    BusyMessage(String),
    ErrorMessage(String, String),
    AsyncDialog {
        title: String,
        body: String,
        #[derivative(PartialEq = "ignore")]
        sender: Sender<usize>,
        options: Vec<String>,
        key: AsyncDialogKey,
        strategy: AsyncDialogStrategy,
    },
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AsyncDialogStrategy {
    /// Clear any dialogs in the stack with the same ID
    Replace,
    /// Push it onto the stack,
    /// but do not display if it is not the lowest one
    /// with the given ID
    Queue,
    /// Display dialog regardless of the existence
    /// of others with the same ID
    #[default]
    Stack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AsyncDialogKey {
    GameFind,
    LocalFind,
    DownloadAllConfirm,
}

impl AppModal {
    fn can_escape(&self) -> bool {
        use AppModal::*;
        match self {
            ApiKeyRequest => false,
            AddToProfileRequest(_) => true,
            LibraryDeleteRequest => true,
            SteamGuardCodeRequest => false,
            ProfileAddRequest => true,
            ItemDetailedView(_) => true,
            Busy => false,
            BusyMessage(_) => false,
            ErrorMessage(_, _) => true,
            AsyncDialog { .. } => false,
        }
    }

    fn async_dialog<T: Into<String>, B: Into<String>>(
        key: AsyncDialogKey,
        title: T,
        body: B,
        options: Vec<String>,
        strategy: AsyncDialogStrategy,
    ) -> (Self, Receiver<usize>) {
        let (sender, receiver) = iced::futures::channel::mpsc::channel(1);
        (
            Self::AsyncDialog {
                key,
                title: title.into(),
                body: body.into(),
                sender,
                options,
                strategy,
            },
            receiver,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppAbortKey {
    GameFind,
    LocalFind,
    GameLogMonitor,

    Id(usize),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AppSave {
    username: String,
    profiles: BTreeMap<usize, library::Profile>,
    active_profile: Option<usize>,
    game_directory: Option<PathBuf>,
    local_directory: Option<PathBuf>,
    launch_command: Option<String>,
    launch_args: Vec<(String, String)>,
}

pub struct App {
    #[cfg(target_os = "linux")]
    dbus_connection: zbus::blocking::Connection,

    api_key: SecretString,
    current_page: AppPage,
    modal_stack: Vec<AppModal>,
    browsing_page: u32,
    browsing_page_max: u32,
    browsing_query: web::WorkshopQuery,
    browsing_query_period: web::WorkshopTrendPeriod,
    browse_query_edit: String,
    browse_query_tags_open: bool,
    loaded_files: Vec<steam_rs::published_file_service::query_files::File>,
    steamcmd_state: Arc<steamcmd::State>,
    steamcmd_code: SecretString,
    settings: AppSettings,
    settings_editing: AppSettings,
    credentials: AppCredentials,
    steamcmd_log: iced::widget::text_editor::Content,
    save: AppSave,
    images: HashMap<String, image::Handle>,
    download_queue: RingMap<u32, u64>,
    ongoing_downloads: RingMap<u32, u64>,
    completed_downloads: BTreeSet<u32>,
    errorred_downloads: BTreeMap<u32, String>,
    cancel_download_handles: HashMap<u32, iced::task::Handle>,
    downloaded: BTreeMap<u32, PathBuf>,
    // Potential todo: cache results? Doesn't seem strictly necessary
    metadata: BTreeMap<u32, xcom_mod::ModMetadata>,
    library: library::Library,
    file_cache: files::Cache,
    markup_cache: markup::MarkupCache,
    profile_add_name: String,
    selected_profile_id: Option<usize>,
    background_resolver_sender: Option<Sender<Message>>,
    mod_editor: mod_edit::Editor,
    active_profile_combo: combo_box::State<String>,
    abortable_handles: HashMap<AppAbortKey, iced::task::Handle>,
    launch_log: iced::widget::text_editor::Content,
}

#[derive(Debug, Reflect, Clone, Copy)]
struct AppSettingsUnsignedRange {
    min: u32,
    max: u32,
}

impl Default for AppSettingsUnsignedRange {
    fn default() -> Self {
        Self {
            min: u32::MIN,
            max: u32::MAX,
        }
    }
}

#[derive(Debug, Reflect, Clone, Copy)]
enum AppSettingEditor {
    StringInput,
    SecretInput,
    NumberInput,
    BoolToggle,
}

#[derive(Debug, Clone)]
pub enum AppSettingEdit {
    String(&'static str, String),
    Secret(&'static str, String),
    Number(&'static str, String),
    Bool(&'static str, bool),
}

impl AppSettingEditor {
    fn get_default_for(type_info: &'static TypeInfo) -> AppSettingEditor {
        macro_rules! is_of {
            ($ty:ident,$type:ty) => {
                $ty == Type::of::<$type>()
            };
        }
        match *type_info.ty() {
            ty if is_of!(ty, String) => AppSettingEditor::StringInput,
            ty if is_of!(ty, bool) => AppSettingEditor::BoolToggle,
            ty if is_of!(ty, u32) => AppSettingEditor::NumberInput,
            _ => {
                unimplemented!(
                    "default editor is not specified for type {}",
                    type_info.type_path()
                );
            }
        }
    }
}

#[derive(Debug, Reflect)]
struct AppSettingsLabel(&'static str);

#[derive(Debug, Reflect)]
struct AppSettingsDescription(&'static str);

#[derive(Debug, Reflect)]
struct AppSettingsDepends(Vec<&'static str>);

#[derive(Debug, Reflect)]
struct AppSettingsConflicts(Vec<&'static str>);

#[derive(Debug, Reflect)]
struct AppSettingsTimePreview;

#[derive(Debug, Reflect, PartialEq, Serialize, Deserialize, Clone)]
#[serde(default)]
struct AppSettings {
    #[reflect(@AppSettingsLabel("Download Directory"))]
    #[reflect(@AppSettingsDescription("Note: Already downloaded files are not automatically moved if you change this value."))]
    download_directory: String,

    #[reflect(@AppSettingsLabel("Simultaneous Downloads"))]
    #[reflect(@AppSettingsDescription("Maximum amount of allowed ongoing downloads at any given moment."))]
    #[reflect(@AppSettingsUnsignedRange { min: 1, max: 10 })]
    simultaneous_downloads: u32,

    #[reflect(@AppSettingsLabel("Automatic Download Retries"))]
    #[reflect(@AppSettingsDescription(r#"Maximum amount of retries when attempting to download an item.
This is largely relevant for large mods where steamcmd will simply timeout the connection and stop the download,
although this will also retry if any other errors occur. Set this to a higher value if you are finding that mods fail to download consistently."#))]
    #[reflect(@AppSettingsUnsignedRange { min: 0, max: 20 })]
    automatic_download_retries: u32,

    #[reflect(@AppSettingsLabel("Check Updates on Startup"))]
    check_updates_on_startup: bool,

    #[reflect(@AppSettingsLabel("Notify on Download Complete"))]
    #[reflect(@AppSettingsDescription("If enabled, sends a notification when the download queue becomes empty."))]
    notify_on_download_complete: bool,

    #[reflect(@AppSettingsLabel("Notify with Sound"))]
    #[reflect(@AppSettingsDescription("If enabled, plays a sound when the download notification is sent."))]
    notify_with_sound: bool,

    #[reflect(@AppSettingsLabel("Show Download Progress Notification"))]
    notify_progress: bool,

    #[reflect(@AppSettingsLabel("Workshop Update Item Id"))]
    #[reflect(@AppSettingsDescription(r#"Due to strange steamcmd behavior which does not appear to be addressed anywhere,
at least one workshop item must be (successfully) downloaded before the "+workshop_status" command works.
By default, this is set to X2WOTCCommunityHighlander's ID since it is a common dependency."#))]
    workshop_update_item_id: u32,

    #[reflect(@AppSettingsLabel("Reload Profile on Launch"))]
    #[reflect(@AppSettingsDescription("If enabled, automatically sets up the selected active profile."))]
    reload_profile_on_launch: bool,

    #[reflect(@AppSettingsLabel("steamcmd: Command Path"))]
    #[reflect(@AppSettingsDescription(r#"If empty (default), attempts to find it in your path using 'which "steamcmd"'"#))]
    steamcmd_command_path: String,
    #[reflect(@AppSettingsLabel("steamcmd: Logout On Exit"))]
    #[reflect(@AppSettingsDescription("If enabled, runs 'steamcmd +login [username] +logout + quit' when closing, necessitating that you log in again when running the app."))]
    steamcmd_logout_on_exit: bool,
    #[reflect(@AppSettingsLabel("steamcmd: Save Password"))]
    #[reflect(@AppSettingsDescription("If enabled, saves your Steam password to your secrets wallet to be used when opening the app again."))]
    steamcmd_save_password: bool,

    #[reflect(@AppSettingsLabel("Steam Web API: Save API Key"))]
    #[reflect(@AppSettingsDescription("If enabled, saves your Steam web API key to your secrets wallet to be used when opening the app again."))]
    steam_webapi_save_api_key: bool,
    #[reflect(@AppSettingsLabel("Steam Web API: Query Cache Lifetime (seconds)"))]
    #[reflect(@AppSettingsUnsignedRange { min: 0, max: 604800 })]
    #[reflect(@AppSettingsTimePreview)]
    steam_webapi_cache_lifetime: u32,
}

static APP_SETTINGS_INFO: LazyLock<&StructInfo> = LazyLock::new(|| {
    AppSettings::type_info()
        .as_struct()
        .expect("AppSettings should be a struct")
});

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            download_directory: DATA_DIR.join("downloads").display().to_string(),
            simultaneous_downloads: 1,
            automatic_download_retries: 1,
            check_updates_on_startup: true,
            notify_on_download_complete: true,
            notify_with_sound: false,
            notify_progress: false,
            workshop_update_item_id: X2_WOTCCOMMUNITY_HIGHLANDER_ID,
            reload_profile_on_launch: true,

            steamcmd_command_path: Default::default(),
            steamcmd_logout_on_exit: false,
            steamcmd_save_password: false,

            steam_webapi_save_api_key: false,
            steam_webapi_cache_lifetime: 86400,
        }
    }
}

#[derive(Debug)]
struct AppCredentials {
    steam_web_api: keyring::Entry,
    steam_password: keyring::Entry,
}

impl App {
    fn theme(&self) -> Theme {
        Theme::Ferra
    }

    fn boot(
        #[cfg(target_os = "linux")] connection: zbus::blocking::Connection,
    ) -> eyre::Result<(Self, Task<Message>)> {
        let steam_web_api_entry = keyring::Entry::new("lxcomm-steam", "steam-web-api")?;
        let steam_password_entry = keyring::Entry::new("lxcomm-steam", "steam-password")?;

        let settings = if let Ok(true) = SETTINGS_PATH.try_exists()
            && let Ok(file) = std::fs::File::open(&*SETTINGS_PATH)
            && let Ok(settings) = serde_json::from_reader(file)
        {
            settings
        } else {
            AppSettings::default()
        };

        let ongoing_downloads = RingMap::with_capacity(settings.simultaneous_downloads as usize);

        let save = if let Ok(true) = SAVE_PATH.try_exists()
            && let Ok(file) = std::fs::File::open(&*SAVE_PATH)
            && let Ok(save) = serde_json::from_reader(file)
        {
            save
        } else {
            AppSave::default()
        };

        let steamcmd_state = Arc::new(steamcmd::State {
            username: save.username.clone(),
            password: SecretString::from(
                steam_password_entry.get_password().ok().unwrap_or_default(),
            ),
            download_dir: PathBuf::from(&settings.download_directory),
            command_path: settings
                .steamcmd_command_path
                .is_empty()
                .not()
                .then_some(settings.steamcmd_command_path.clone().into()),
            // Always assume it is cached and then elevate if it isn't
            is_cached: true,
            ..Default::default()
        });

        // Overwrite by default in case there were missing fields
        let result: Result<()> = try {
            std::fs::create_dir_all(&*CONFIG_DIR)?;
            let file = std::fs::File::create(&*SETTINGS_PATH)?;
            serde_json::to_writer_pretty(file, &settings)?;
        };
        if let Err(err) = result {
            eprintln!("Failed to overwrite settings on boot: {err}");
        };

        let active_profile_combo =
            combo_box::State::new(save.profiles.values().map(|p| p.name.clone()).collect());

        let mut app = App {
            #[cfg(target_os = "linux")]
            dbus_connection: connection,

            api_key: SecretString::default(),
            current_page: Default::default(),
            modal_stack: Vec::new(),
            browsing_page: 0,
            browsing_page_max: 0,
            browsing_query: web::WorkshopQuery::default(),
            browsing_query_period: web::WorkshopTrendPeriod::default(),
            browse_query_edit: String::new(),
            browse_query_tags_open: false,
            loaded_files: vec![],
            steamcmd_state,
            steamcmd_code: SecretString::default(),
            settings_editing: settings.clone(),
            settings,
            save,
            credentials: AppCredentials {
                steam_web_api: steam_web_api_entry,
                steam_password: steam_password_entry,
            },
            steamcmd_log: Default::default(),
            images: HashMap::new(),
            download_queue: RingMap::new(),
            ongoing_downloads,
            completed_downloads: BTreeSet::new(),
            errorred_downloads: BTreeMap::new(),
            cancel_download_handles: HashMap::new(),
            downloaded: BTreeMap::new(),
            metadata: BTreeMap::new(),
            library: library::Library::default(),
            file_cache: files::Cache::default(),
            markup_cache: markup::MarkupCache::default(),
            profile_add_name: String::new(),
            selected_profile_id: None,
            background_resolver_sender: None,
            mod_editor: mod_edit::Editor::default(),
            active_profile_combo,
            abortable_handles: HashMap::new(),
            launch_log: Default::default(),
        };

        let auto_grab_api_key = match app.credentials.steam_web_api.get_password() {
            _ if !app.settings.steam_webapi_save_api_key => Task::done(Message::ApiKeyRequest),

            Ok(key) if key.is_empty() => Task::done(Message::ApiKeyRequest),
            Ok(key) => {
                app.api_key = SecretString::from(key);
                Task::none()
            }
            Err(err) => {
                let task = Task::done(Message::ApiKeyRequest);
                if let keyring::Error::NoEntry = err {
                    task
                } else {
                    task.chain(Task::done(Message::DisplayError(
                        "Error Loading API Key".to_string(),
                        err.to_string(),
                    )))
                }
            }
        };
        let auto_login = Task::done(Message::SteamCMDLogin(false));

        app.scan_downloads();

        Ok((app, Task::batch([auto_grab_api_key, auto_login])))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Settings(message) => return self.update_settings(message),
            Message::ModEditor(message) => {
                if let Some(task) = self.mod_editor.update(message) {
                    return task;
                }
            }
            Message::AsyncDialogResolve(key, choice) => {
                if let Some(index) =
                    self.modal_stack
                        .iter()
                        .enumerate()
                        .find_map(|(index, modal)| {
                            matches!(modal, AppModal::AsyncDialog { key: this_key, .. } if *this_key == key).then_some(index)
                        })
                    && let AppModal::AsyncDialog { mut sender, .. } = self.modal_stack.remove(index)
                {
                    return Task::future(async move {
                        if let Err(err) = sender.send(choice).await {
                            eprintln!("Error resolving async dialog: {err:?}")
                        }
                        Message::None
                    });
                }
            }
            Message::AsyncDialogCancelled(key) => {
                self.modal_stack.retain(|modal| !matches!(modal, AppModal::AsyncDialog { key: this, .. } if *this == key));
            }
            Message::AbortTask(key) => {
                if let Some(handle) = self.abortable_handles.remove(&key) {
                    handle.abort()
                }
            }

            Message::FileWatchEvent(event) => return self.file_watch_event(event),
            Message::BackgroundResolverMessage(message) => match message {
                web::ResolverMessage::Setup(sender) => {
                    let task = {
                        let mut sender = sender.clone();
                        let key = {
                            let key = self.api_key.expose_secret().to_owned();
                            if key.is_empty() { None } else { Some(key) }
                        };
                        Task::perform(
                            async move {
                                if let Err(err) = sender
                                    .send(Message::BackgroundResolverMessage(
                                        web::ResolverMessage::UpdateClient(key),
                                    ))
                                    .await
                                {
                                    eprintln!(
                                        "Error sending API key to background thread: {err:?}"
                                    );
                                }
                            },
                            |_| Message::None,
                        )
                    };
                    self.background_resolver_sender = Some(sender);
                    return task;
                }
                web::ResolverMessage::Resolved(files) => {
                    let mut tasks = Vec::new();
                    for details in files.iter() {
                        if let Ok(id) = details.published_file_id.parse::<u32>()
                            && let Err(err) = self.file_cache.insert_details(id, details)
                        {
                            eprintln!("Error writing file details to cache: {err:?}");
                        }

                        tasks.push(self.cache_item_image(details));
                    }
                    return Task::batch(tasks);
                }
                _ => (),
            },

            Message::Chained(batched) => {
                let mut task = Task::none();
                for message in batched {
                    task = task.chain(self.update(message));
                }
                return task;
            }

            Message::QueryFilesLoaded(files) => {
                self.loaded_files = files.published_file_details;
                self.browsing_page_max = files.total as u32 / web::QUERY_PAGE_SIZE + 1;

                let mut tasks = Vec::new();
                for details in self.loaded_files.iter() {
                    if let Ok(id) = details.published_file_id.parse::<u32>()
                        && let Err(err) = self.file_cache.insert_details(id, details)
                    {
                        eprintln!("Error writing file details to cache: {err:?}");
                    }

                    tasks.push(self.cache_item_image(details));
                }
                return Task::batch(tasks);
            }

            Message::ApiKeyRequest => self.modal_stack.push(AppModal::ApiKeyRequest),
            Message::ApiKeyRequestUpdate(key) => {
                self.api_key = SecretString::from(key);
            }
            Message::ApiKeySubmit => {
                if !self.api_key.expose_secret().is_empty() {
                    self.modal_stack.clear();
                    if let Err(err) = self
                        .credentials
                        .steam_web_api
                        .set_password(self.api_key.expose_secret())
                    {
                        eprintln!("Error writing steam web api key to credentials: {err}");
                    }

                    if let Some(mut sender) = self.background_resolver_sender.clone() {
                        let key = self.api_key.expose_secret().to_owned();
                        return Task::future(async move {
                            if let Err(err) = sender
                                .send(Message::BackgroundResolverMessage(
                                    web::ResolverMessage::UpdateClient(Some(key)),
                                ))
                                .await
                            {
                                eprintln!("Error sending api key to background thread: {err:?}");
                            }
                            Message::None
                        });
                    }
                }
            }
            Message::SetApiKey(key) => {
                self.api_key = SecretString::from(key);
            }

            Message::SetBrowsePage(new) => {
                let new = std::cmp::max(new, 1);
                if new == self.browsing_page {
                    return Task::none();
                }

                self.browsing_page = new;
                return Task::done(Message::BrowseUpdateQuery);
            }
            Message::SetViewingItem(id) => {
                self.modal_stack.push(AppModal::ItemDetailedView(id));
                if let Some(info) = self.file_cache.get_details(id) {
                    self.markup_cache.cache_markup(info.get_description());

                    let unknown = info
                        .children
                        .iter()
                        .filter_map(|c| {
                            let id = c
                                .published_file_id
                                .parse::<u32>()
                                .expect("ids should be numbers");
                            self.file_cache.get_details(id).is_none().then_some(id)
                        })
                        .collect::<Vec<_>>();

                    if !unknown.is_empty()
                        && let Some(mut sender) = self.background_resolver_sender.clone()
                    {
                        return Task::batch([
                            Task::perform(
                                async move {
                                    if let Err(err) = sender
                                        .send(Message::BackgroundResolverMessage(
                                            web::ResolverMessage::RequestResolve(unknown),
                                        ))
                                        .await
                                    {
                                        eprintln!(
                                            "Error sending resolve request to background thread: {err:?}"
                                        );
                                    }
                                },
                                |()| Message::None,
                            ),
                            self.cache_item_image(info.as_ref()),
                        ]);
                    } else {
                        return self.cache_item_image(info.as_ref());
                    }
                } else if let Some(mut sender) = self.background_resolver_sender.clone() {
                    return Task::future(async move {
                        if let Err(err) = sender
                            .send(web::ResolverMessage::RequestResolve(vec![id]).into())
                            .await
                        {
                            eprintln!(
                                "Error sending resolve request to background thread: {err:?}"
                            );
                        }
                        Message::None
                    });
                }
            }
            Message::BrowseEditQuery(query) => self.browse_query_edit = query,
            Message::BrowseEditSort(sort) => self.browsing_query.sort = sort,
            Message::BrowseEditTag(tag) => {
                if self.browsing_query.tags.contains(&tag) {
                    self.browsing_query.tags.remove(&tag);
                } else {
                    self.browsing_query.tags.insert(tag);
                }
            }
            Message::BrowseEditPeriod(period) => {
                self.browsing_query_period = period;
                if let web::WorkshopSort::Trend(_) = self.browsing_query.sort {
                    self.browsing_query.sort = web::WorkshopSort::Trend(period);
                }
            }
            Message::BrowseToggleTagsDropdown(toggled) => self.browse_query_tags_open = toggled,
            Message::BrowseSubmitQuery => {
                self.browsing_query.query = self.browse_query_edit.clone();
                self.browsing_page = 1;

                return Task::done(Message::BrowseUpdateQuery);
            }
            Message::BrowseUpdateQuery => {
                let page = self.browsing_page;
                let query = self.browsing_query.clone();
                let cache_lifetime = self.settings.steam_webapi_cache_lifetime;
                let api_key = self.api_key.clone();
                return Task::done(Message::SetBusy(true))
                    .chain(Task::future(async move {
                        match web::query_mods(
                            Steam::new(api_key.expose_secret()),
                            page,
                            query,
                            cache_lifetime,
                        )
                        .await
                        {
                            Ok((files, from_cache)) => {
                                if !from_cache {
                                    // Wait at least half a second to prevent over activity
                                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                }

                                Message::QueryFilesLoaded(files)
                            }
                            Err(e) => {
                                Message::DisplayError("Page Load Failed".to_string(), e.to_string())
                            }
                        }
                    }))
                    .chain(Task::done(Message::SetBusy(false)));
            }

            Message::SetPage(new) => {
                if let AppPage::Settings = self.current_page
                    && self.settings_editing != self.settings
                {
                    self.modal_stack.push(AppModal::ErrorMessage(
                        "Warning".to_string(),
                        "Unsaved settings are not automatically applied".to_string(),
                    ));
                }
                self.current_page = new;
            }
            Message::OpenModal(modal) => {
                self.modal_stack.push(Arc::unwrap_or_clone(modal));
            }
            Message::CloseModal => {
                self.modal_stack.pop();
            }
            Message::EscapeModal => {
                if self
                    .modal_stack
                    .last()
                    .is_some_and(|modal| modal.can_escape())
                {
                    self.modal_stack.pop();
                }
            }
            Message::SetBusyMessage(message) => {
                self.modal_stack.push(AppModal::BusyMessage(message));
            }
            Message::SetBusy(busy) => {
                if busy {
                    self.modal_stack.push(AppModal::Busy);
                } else {
                    // Realistically, only one "busy" operation should be ongoing at a single time,
                    // with setting "unbusy" clearing the overall "busy-ness" of the application
                    self.modal_stack.retain_mut(|modal| {
                        !matches!(modal, AppModal::Busy | AppModal::BusyMessage(_))
                    });
                }
            }
            Message::DisplayError(title, message) => self
                .modal_stack
                .push(AppModal::ErrorMessage(title, message)),

            Message::SteamCMDLoginSuccess => {
                self.modal_stack
                    .retain(|m| !matches!(m, AppModal::SteamGuardCodeRequest));
                if let Some(state) = Arc::get_mut(&mut self.steamcmd_state) {
                    state.logged_in = true;
                    state.is_cached = true;
                } else {
                    eprintln!("Failed to get exclusive ownership of steamcmd state...");
                }

                return self.queue_downloads();
            }
            Message::SteamCMDLogin(manual) => {
                if self.steamcmd_state.logged_in {
                    return Task::none();
                }

                if manual {
                    if self.steamcmd_state.password.expose_secret().is_empty() {
                        return Task::done(Message::DisplayError(
                            "Error Logging In".to_string(),
                            "Password field is empty".to_string(),
                        ));
                    }

                    if !self.steamcmd_state.is_cached {
                        return Task::done(Message::SteamCMDLoginElevated);
                    }
                } else if !self.steamcmd_state.is_cached {
                    return Task::none();
                }

                self.modal_stack
                    .push(AppModal::BusyMessage("Checking steamcmd login...".into()));
                let state = self.steamcmd_state.clone();
                return Task::perform(
                    async move { state.attempt_cached_login().await },
                    move |result| {
                        if let Err(e) = result {
                            match e {
                                steamcmd::Error::LoginFailure if manual => {
                                    Message::SteamCMDLoginElevated
                                }
                                steamcmd::Error::LoginFailure => Message::None,
                                err => Message::DisplayError(
                                    "Error Logging In".into(),
                                    err.to_string(),
                                ),
                            }
                        } else {
                            Message::SteamCMDLoginSuccess
                        }
                    },
                )
                .chain(Task::done(Message::SetBusy(false)));
            }
            Message::SteamCMDLoginElevated => {
                self.steamcmd_code = SecretString::default();
                if let Some(state) = Arc::get_mut(&mut self.steamcmd_state) {
                    state.is_cached = false;
                }
                self.modal_stack.push(AppModal::SteamGuardCodeRequest);
            }
            Message::SteamCMDLoginElevatedSubmit => {
                self.modal_stack.push(AppModal::Busy);
                let state = self.steamcmd_state.clone();
                let code = self.steamcmd_code.clone();
                self.steamcmd_code = SecretString::from("");
                return Task::perform(
                    async move { state.attempt_full_login(code).await },
                    |result| {
                        if let Err(e) = result {
                            Message::DisplayError("Failed To Log In".to_string(), e.to_string())
                        } else {
                            Message::SteamCMDLoginSuccess
                        }
                    },
                )
                .chain(Task::done(Message::Chained(vec![
                    Message::SetBusy(false),
                    Message::SetSteamCMDCode(String::new()),
                ])));
            }
            Message::SteamCMDLoginElevatedCancel => {
                self.steamcmd_code = SecretString::default();
                self.modal_stack.clear();
            }
            Message::SetSteamCMDUser(user) => {
                self.save.username = user.clone();
                if let Some(state) = Arc::get_mut(&mut self.steamcmd_state) {
                    state.username = user;
                }
            }
            Message::SetSteamCMDPassword(pass) => {
                if let Some(state) = Arc::get_mut(&mut self.steamcmd_state) {
                    state.password = SecretString::from(pass)
                }
            }
            Message::SetSteamCMDCode(code) => self.steamcmd_code = SecretString::from(code),
            Message::DeleteRequested(id) => {
                if let Err(err) = std::fs::remove_dir_all(files::get_item_directory(
                    &self.settings.download_directory,
                    id,
                )) {
                    self.modal_stack.push(AppModal::ErrorMessage(
                        "Error Deleting Item".to_string(),
                        format!("An error occurred while trying to delete {id}:\n{err:?}"),
                    ));
                }
                self.completed_downloads.remove(&id);
                self.downloaded.remove(&id);
                self.library.items.remove(&id);
            }
            Message::DownloadCancelRequested(id) => {
                #[cfg(target_os = "linux")]
                if self.settings.notify_progress
                    && let Some(notif_id) = NOTIF_CACHE.remove(&id)
                    && let Some(details) = self.file_cache.get_details(id)
                {
                    let mut notif = notify_rust::Notification::new();
                    let title = &details.title;
                    notif
                        .appname("LXCOMM")
                        .summary(&format!("{title} Not Downloaded"))
                        .body("Download was cancelled")
                        .timeout(-1)
                        .id(notif_id);
                    let _ = notif.show();
                }

                if let Some(_progress) = self.ongoing_downloads.remove(&id) {
                    if let Some(handle) = self.cancel_download_handles.remove(&id) {
                        handle.abort();
                    }
                    self.errorred_downloads.insert(id, "Cancelled".to_string());
                } else if let Some(_progress) = self.download_queue.remove(&id) {
                    self.errorred_downloads.insert(id, "Cancelled".to_string());
                }
            }
            Message::SteamCMDDownloadRequested(id) => {
                if !self.steamcmd_state.logged_in {
                    return Task::done(Message::DisplayError(
                        "Error: Not Logged In".to_string(),
                        "You must be logged in to steamcmd to download files.".to_string(),
                    ));
                }

                if self.is_downloading(id) {
                    return Task::none();
                }

                self.errorred_downloads.remove(&id);

                if self.ongoing_downloads.len() < self.settings.simultaneous_downloads as usize {
                    return self.perform_download(id);
                }

                self.download_queue.insert(id, 0);
            }
            Message::SteamCMDDownloadCompleted(id) => {
                self.ongoing_downloads.remove(&id);
                self.completed_downloads.insert(id);
                self.scan_downloads();

                #[cfg(target_os = "linux")]
                if self.settings.notify_progress
                    && let Some(notif_id) = NOTIF_CACHE.remove(&id)
                    && let Some(details) = self.file_cache.get_details(id)
                {
                    let mut notif = notify_rust::Notification::new();
                    let title = &details.title;
                    notif
                        .appname("LXCOMM")
                        .summary(&format!("Downloaded {title}"))
                        .body("Downloaded Completed")
                        .timeout(-1)
                        .id(notif_id);
                    let _ = notif.show();
                }

                if self.ongoing_downloads.is_empty() && self.download_queue.is_empty() {
                    if self.settings.notify_on_download_complete {
                        let sound_name = if self.settings.notify_with_sound {
                            "complete-download"
                        } else {
                            ""
                        }
                        .to_string();
                        return Task::future(async move {
                            let mut notif = notify_rust::Notification::new();
                            notif
                                .appname("LXCOMM")
                                .summary("Downloads Complete")
                                .body("All downloads in the queue have completed")
                                .sound_name(&sound_name)
                                .timeout(-1);

                            #[cfg(target_os = "linux")]
                            notif
                                .hint(notify_rust::Hint::Category("TransferComplete".to_string()))
                                .hint(notify_rust::Hint::Resident(true));

                            #[cfg(target_os = "linux")]
                            let result = notif.show_async().await;
                            #[cfg(not(target_os = "linux"))]
                            let result = tokio::task::spawn_blocking(move || notif.show()).await;

                            if let Err(err) = result {
                                eprintln!("Error sending notification: {err:?}");
                            }
                            Message::None
                        });
                    } else {
                        return Task::none();
                    }
                }

                return self.queue_downloads();
            }
            Message::SteamCMDDownloadErrored(id, message) => {
                #[cfg(target_os = "linux")]
                if self.settings.notify_progress
                    && let Some(notif_id) = NOTIF_CACHE.remove(&id)
                    && let Some(details) = self.file_cache.get_details(id)
                {
                    let mut notif = notify_rust::Notification::new();
                    let title = &details.title;
                    notif
                        .appname("LXCOMM")
                        .summary(&format!("Error Downloading {title}"))
                        .body(&message)
                        .timeout(-1)
                        .id(notif_id);
                    let _ = notif.show();
                }

                self.ongoing_downloads.remove(&id);
                self.errorred_downloads.insert(id, message);

                return self.queue_downloads();
            }
            Message::SteamCMDDownloadProgress(id, size) => {
                self.ongoing_downloads.entry(id).and_modify(|e| *e = size);

                #[cfg(target_os = "linux")]
                if self.settings.notify_progress
                    && let Some(info) = self.file_cache.get_details(id)
                    && let Ok(total_size) = info.file_size.parse::<u64>()
                {
                    let title = info.title.clone();
                    let progress = std::cmp::min(size * 100 / total_size, 0);
                    return Task::future(async move {
                        let mut notif = notify_rust::Notification::new();
                        notif
                            .appname("LXCOMM")
                            .summary(&format!("Downloading {title}"))
                            .body(&format!(
                                "{} of {} Downloaded",
                                files::SizeDisplay::automatic_from(size, total_size),
                                files::SizeDisplay::automatic(total_size)
                            ))
                            .timeout(0)
                            .hint(notify_rust::Hint::Resident(true))
                            .hint(notify_rust::Hint::CustomInt(
                                "value".to_string(),
                                progress as i32,
                            ));

                        if let Some(notif_id) = NOTIF_CACHE.get(&id) {
                            notif.id(notif_id);
                        }

                        if let Ok(handle) = notif.show_async().await {
                            NOTIF_CACHE.insert(id, handle.id());
                        }

                        Message::None
                    });
                }
            }
            Message::SteamCMDDownloadCompletedClear(ids) => {
                for id in ids {
                    self.completed_downloads.remove(&id);
                }
            }
            Message::SteamCMDDownloadErrorClear(ids) => {
                for id in ids {
                    self.errorred_downloads.remove(&id);
                }
            }
            Message::SteamCMDCheckUpdateRequested => {
                let ids = self.library.items.keys().copied().collect::<Vec<_>>();
                let cache = self.file_cache.clone();
                let state = self.steamcmd_state.clone();

                return Task::future(async move {
                    match state.check_updates(ids, cache).await {
                        Err(err) => {
                            Message::display_error("Error Checking Updates", format!("{err:#?}"))
                        }
                        Ok(ids) => Message::SteamCMDCheckUpdateCompleted(ids),
                    }
                });
            }
            Message::SteamCMDCheckUpdateCompleted(ids) => {
                println!("The following ids need to be updated: {ids:#?}");
            }
            Message::DownloadAllRequested(id) => {
                let client = Steam::new(self.api_key.expose_secret());
                let cache = self.file_cache.clone();
                let downloaded = self.downloaded.keys().cloned().collect::<Vec<_>>();
                return Task::done(Message::SetBusyMessage(
                    "Resolving Dependencies...".to_string(),
                ))
                .chain(Task::future({let cache = cache.clone(); async move {
                    web::resolve_all_dependencies(id, client, cache).await
                }}).then(move |result| {
                    match result {
                        Ok(to_download) => {
                            let mut to_download = Arc::unwrap_or_clone(to_download);
                            to_download.retain(|id| !downloaded.contains(id));
                            if to_download.is_empty() {
                                return Task::done(Message::display_error(
                                    "Already Downloaded", 
                                    "All dependencies listed by Steam have already been downloaded."
                                ));
                            }

                            let list = to_download.iter().map(|&id| {
                                format!("{} ({id})", cache.get_details(id).as_deref().map(|f| f.title.as_str()).unwrap_or("UNKNOWN"))
                            }).join("\n");
                            let (modal, mut rec) = AppModal::async_dialog(
                                AsyncDialogKey::DownloadAllConfirm,
                                "Download All?",
                                format!("The following items have not been downloaded yet:\n{list}"),
                                vec!["No".to_string(), "Yes".to_string()],
                                AsyncDialogStrategy::Replace,
                            );
                            Task::done(Message::OpenModal(Arc::new(modal))).chain(Task::future(async move {
                                match rec.next().await {
                                    Some(1) => Message::DownloadAllConfirmed(Arc::new(to_download)),
                                    _ => Message::None
                                }
                            }))
                        }
                        Err(err) => {
                            Task::done(Message::display_error("Error Resolving Dependencies", format!(
                                "An error occured while trying to resolve dependencies. Original error:\n{err:?}"
                            )))
                        }
                    }
                })).chain(Task::done(Message::SetBusy(false)));
            }
            Message::DownloadAllConfirmed(ids) => {
                return Task::batch(
                    ids.iter()
                        .map(|id| Task::done(Message::SteamCMDDownloadRequested(*id))),
                );
            }

            Message::LibraryToggleAll(toggle) => {
                self.library
                    .iter_filtered_mut()
                    .for_each(|item| item.selected = toggle);
            }
            Message::LibraryToggleItem(id, toggle) => {
                self.library
                    .items
                    .entry(id)
                    .and_modify(|item| item.selected = toggle);
            }
            Message::LibraryUpdateRequest => {
                if !self.steamcmd_state.logged_in {
                    return Task::done(Message::DisplayError(
                        "Error".to_string(),
                        "You must log in to steamcmd in order to update!".to_string(),
                    ));
                }

                for item in self.library.iter_selected() {
                    self.download_queue.push_back(item.id, 0);
                }
                return self.queue_downloads();
            }
            Message::LibraryScanRequest => {
                self.scan_downloads();
            }
            Message::LibraryAddToProfileRequest => {
                for profile in self.save.profiles.values_mut() {
                    profile.add_selected = false;
                }
                self.modal_stack.push(AppModal::AddToProfileRequest(
                    self.library.items.keys().copied().collect(),
                ));
            }
            Message::LibraryAddToProfileToggleAll(toggled) => {
                self.save
                    .profiles
                    .values_mut()
                    .for_each(|p| p.add_selected = toggled);
            }
            Message::LibraryAddToProfileToggled(id, toggle) => {
                self.save
                    .profiles
                    .entry(id)
                    .and_modify(|profile| profile.add_selected = toggle);
            }
            Message::LibraryAddToProfileConfirm(ids) => {
                for profile in self
                    .save
                    .profiles
                    .values_mut()
                    .filter(|profile| profile.add_selected)
                {
                    for &item in ids.iter() {
                        profile
                            .items
                            .entry(item)
                            .or_insert_with(library::LibraryItemSettings::default);
                    }
                    profile.update_compatibility_issues(
                        self.file_cache.clone(),
                        &self.metadata,
                        &self.library,
                    );
                }
                self.modal_stack.pop();
            }
            Message::LibraryDeleteRequest => self.modal_stack.push(AppModal::LibraryDeleteRequest),
            Message::LibraryDeleteConfirm => {
                self.modal_stack.pop();
                return Task::batch(
                    self.library
                        .iter_selected()
                        .map(|item| Task::done(Message::DeleteRequested(item.id))),
                );
            }
            Message::LibraryFilterUpdateQuery(query) => {
                self.library.filter_query = query;
                self.library.update_selected_filtered();
            }
            Message::LibraryFilterToggleFuzzy(toggle) => {
                self.library.filter_fuzzy = toggle;
                self.library.update_selected_filtered();
            }

            Message::ProfileAddPressed => {
                self.profile_add_name = String::new();
                self.modal_stack.push(AppModal::ProfileAddRequest);
            }
            Message::ProfileAddCompleted(submitted) => {
                if submitted {
                    if self.profile_add_name.is_empty() {
                        return Task::none();
                    }

                    if self
                        .save
                        .profiles
                        .iter()
                        .any(|(_id, profile)| profile.name == self.profile_add_name)
                    {
                        return Task::done(Message::DisplayError(
                            "Error".to_string(),
                            "Profile with this name already exists!".to_string(),
                        ));
                    }

                    let id = (0..usize::MAX)
                        .find(|i| !self.save.profiles.contains_key(i))
                        .expect("profile should never reasonably contain > usize::MAX items");

                    self.save.profiles.insert(
                        id,
                        library::Profile {
                            id,
                            name: self.profile_add_name.clone(),
                            items: BTreeMap::new(),
                            ..Default::default()
                        },
                    );
                    self.selected_profile_id = Some(id);
                    self.active_profile_combo = combo_box::State::new(
                        self.save
                            .profiles
                            .values()
                            .map(|p| &p.name)
                            .cloned()
                            .collect(),
                    );
                    let remnants = PROFILES_DIR.join(id.to_string());
                    if let Ok(true) = remnants.try_exists()
                        && let Err(err) = std::fs::remove_dir_all(remnants)
                    {
                        eprintln!("Error deleting left-over profile folder: {err:?}");
                    }
                }
                self.modal_stack.pop();
            }
            Message::ProfileAddEdited(name) => self.profile_add_name = name,
            Message::ProfileAddItems(id, items) => {
                self.save.profiles.entry(id).and_modify(|profile| {
                    profile.items.extend(
                        items
                            .into_iter()
                            .map(|id| (id, library::LibraryItemSettings::default())),
                    );

                    profile.update_compatibility_issues(
                        self.file_cache.clone(),
                        &self.metadata,
                        &self.library,
                    );
                });
            }
            Message::ProfileDeletePressed(id) => {
                self.save.profiles.remove(&id);
                if Some(id) == self.save.active_profile {
                    self.save.active_profile = None;
                }
            }
            Message::ProfileRemoveItems(id, items) => {
                self.save.profiles.entry(id).and_modify(|profile| {
                    profile.items.retain(|id, _| !items.contains(id));
                    profile.update_compatibility_issues(
                        self.file_cache.clone(),
                        &self.metadata,
                        &self.library,
                    );
                });

                if let Some(selected) = self.selected_profile_id
                    && id == selected
                    && let Some(profile) = self.save.profiles.get_mut(&selected)
                    && profile
                        .view_selected_item
                        .as_ref()
                        .map(|id| items.iter().find(|i| *i == id))
                        .is_some()
                {
                    profile.view_selected_item = None;
                }
            }
            Message::ProfileSelected(id) => {
                self.selected_profile_id = Some(id);
                if let Some(profile) = self.save.profiles.get(&id) {
                    let mut tasks = Vec::new();
                    for id in profile.items.keys() {
                        if let Some(details) = self.file_cache.get_details(*id) {
                            self.markup_cache.cache_markup(details.get_description());
                            tasks.push(self.cache_item_image(&details));
                        }
                    }

                    return Task::batch(tasks);
                }
            }
            Message::ProfileItemSelected(id) => {
                if self.mod_editor.has_unsaved() {
                    return Task::done(Message::DisplayError(
                        "Unsaved Changes".to_string(),
                        "Your configurations to the current mod have unsaved changes.".to_string(),
                    ));
                }

                if let Some(selected) = self.selected_profile_id
                    && let Some(profile) = self.save.profiles.get_mut(&selected)
                {
                    if Some(id) == profile.view_selected_item {
                        profile.view_selected_item = None;
                    } else {
                        profile.view_selected_item = Some(id);

                        if let Some(task) = self.mod_editor.update(mod_edit::EditorMessage::Load(
                            selected,
                            id,
                            PathBuf::from(&self.settings.download_directory),
                        )) {
                            return task;
                        }
                    }
                }
            }
            Message::ProfileViewSaveRequested(id) => {
                let path = PROFILES_DIR.join(id.to_string()).join("SaveData");

                if let Err(err) = opener::open(&path) {
                    return Task::done(Message::display_error(
                        "Error Opening",
                        format!("Something went wrong opening {}:\n{err:?}", path.display()),
                    ));
                }
            }
            Message::ProfileImportSaveRequested(id) => {
                return Task::done(Message::SetBusy(true))
                    .chain(Task::future(async move {
                        if let Some(handle) = rfd::AsyncFileDialog::new()
                            .set_title("Pick SaveData Folder")
                            .pick_folder()
                            .await
                        {
                            let path = handle.path();
                            if !path.join("profile.bin").exists() {
                                return Message::display_error(
                                    "Error Copying Save Files", 
                                    format!("{} is missing file 'profile.bin', is this a valid XCOM2 save folder?", path.display())
                                )
                            }

                            let profile_dir = PROFILES_DIR.join(id.to_string());
                            let dest = profile_dir.join("SaveData");
                            let res: Result<(), std::io::Error> = try {
                                if dest.try_exists()? {
                                    tokio::fs::remove_dir_all(&dest).await?;
                                }
                                tokio::fs::create_dir_all(&dest).await?;
                            };

                            if let Err(err) = res {
                                return Message::display_error("Error Copying Save Files", format!("Failed to overwrite save:\n{err:?}"))
                            }

                            if let Err(err) = dircpy::copy_dir(path, &dest) {
                                Message::display_error(
                                    "Error Copying Save Files",
                                    format!("An error occurred while copying save files from '{}' to '{}':\n{err:?}", path.display(), dest.display()),
                                )
                            } else {
                                Message::None
                            }
                        } else {
                            Message::None
                        }
                    }))
                    .chain(Task::done(Message::SetBusy(false)));
            }
            Message::ActiveProfileSelected(name) => {
                self.save.active_profile = self
                    .save
                    .profiles
                    .iter()
                    .find_map(|(id, p)| (p.name == name).then_some(*id))
            }

            Message::LoadPrepareProfile(manual) => {
                let Some(destination) = &self.save.game_directory else {
                    return Task::done(Message::display_error(
                        "No Game Directory",
                        "Game directory must be set.",
                    ));
                };

                let Some(local_path) = &self.save.local_directory else {
                    return Task::done(Message::display_error(
                        "No Local Directory",
                        "Local directory must be set.",
                    ));
                };

                if !destination.exists() {
                    return Task::done(Message::display_error(
                        "Directory Not Found",
                        format!("Could not find game directory at {}", destination.display()),
                    ));
                }
                if !destination.join("XComGame").exists() {
                    return Task::done(Message::display_error(
                        "Invalid Game Directory",
                        format!(
                            "Unable to find the 'XComGame' subdirectory in {}",
                            destination.display()
                        ),
                    ));
                }

                if let Some(active) = self.save.active_profile
                    && let Some(profile) = self.save.profiles.get(&active)
                {
                    if let Err(err) = loading::bootstrap_load_profile(
                        profile,
                        &self.settings.download_directory,
                        &self.metadata,
                        destination,
                        local_path,
                    ) {
                        return Task::done(Message::display_error(
                            "Error Applying Config",
                            format!("{err:#?}"),
                        ));
                    } else if manual {
                        // TODO - More generic dialog name, just using error for now
                        return Task::done(Message::display_error(
                            "Success",
                            "Config was successfully applied.",
                        ));
                    }
                }
            }
            Message::LoadPickGameDirectory => {
                let task = Task::done(Message::SetBusy(true));
                let task = task.chain(Task::future(async {
                    if let Some(handle) = rfd::AsyncFileDialog::new().pick_folder().await {
                        Message::LoadSetGameDirectory(handle.path().to_path_buf())
                    } else {
                        Message::None
                    }
                }));
                return task.chain(Task::done(Message::SetBusy(false)));
            }
            Message::LoadSetGameDirectory(path) => self.save.game_directory = Some(path),
            Message::LoadPickLocalDirectory => {
                let task = Task::done(Message::SetBusy(true));
                let task = task.chain(Task::future(async {
                    if let Some(handle) = rfd::AsyncFileDialog::new().pick_folder().await {
                        Message::LoadSetLocalDirectory(handle.path().to_path_buf())
                    } else {
                        Message::None
                    }
                }));
                return task.chain(Task::done(Message::SetBusy(false)));
            }
            Message::LoadSetLocalDirectory(path) => self.save.local_directory = Some(path),
            Message::LoadPickLaunchCommand => {
                let task = Task::done(Message::SetBusy(true));
                let task = task.chain(Task::future(async {
                    if let Some(handle) = rfd::AsyncFileDialog::new().pick_file().await {
                        Message::LoadSetLaunchCommand(handle.path().display().to_string())
                    } else {
                        Message::None
                    }
                }));
                return task.chain(Task::done(Message::SetBusy(false)));
            }
            Message::LoadSetLaunchCommand(command) if command.is_empty() => {
                self.save.launch_command = None
            }
            Message::LoadSetLaunchCommand(command) => self.save.launch_command = Some(command),
            Message::LoadAddLaunchArgs => {
                self.save.launch_args.push(Default::default());
            }
            Message::LoadRemoveLaunchArgs(i) => {
                self.save.launch_args.remove(i);
            }
            Message::LoadEditArgKey(i, key) => {
                if let Some(k) = self.save.launch_args.get_mut(i).map(|pair| &mut pair.0) {
                    *k = key;
                }
            }
            Message::LoadEditArgValue(i, value) => {
                if let Some(v) = self.save.launch_args.get_mut(i).map(|pair| &mut pair.1) {
                    *v = value;
                }
            }
            Message::LoadLaunchGame => {
                return if let Some(command) = self.save.launch_command.clone() {
                    if self.settings.reload_profile_on_launch {
                        let error_task = self.update(Message::LoadPrepareProfile(false));
                        if error_task.units() > 0 {
                            return error_task;
                        }
                    }

                    let args = self.save.launch_args.clone();
                    let busy = Task::done(Message::SetBusy(true));
                    busy.chain(Task::future(async move {
                        let mut command = std::process::Command::new(command);
                        command.stdout(Stdio::null());

                        for (l, r) in args {
                            if !l.is_empty() {
                                command.arg(l);
                            }
                            command.arg(r);
                        }

                        if let Err(err) = command.spawn() {
                            Message::Chained(vec![
                                Message::SetBusy(false),
                                Message::display_error(
                                    "Error Launching",
                                    format!("Error launching game:\n{err:#?}"),
                                ),
                            ])
                        } else {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            Message::SetBusy(false)
                        }
                    }))
                    .chain(self.setup_launch_log_monitor())
                } else {
                    Task::done(Message::DisplayError(
                        "Missing Command".to_string(),
                        "No command has been set set.".to_string(),
                    ))
                };
            }

            Message::LoadFindGameRequested => {
                let (task, abort) =
                    files::find_directories_matching("XCOM 2/XCom2-WarOfTheChosen/Binaries");

                if let Some(handle) = self.abortable_handles.insert(AppAbortKey::GameFind, abort) {
                    handle.abort();
                }

                return Task::done(Message::SetBusy(true)).chain(task.map(|p| match p {
                    Some(path) => Message::LoadFindGameMatched(path),
                    None => Message::Chained(vec![
                        Message::AbortTask(AppAbortKey::GameFind),
                        Message::SetBusy(false),
                    ]),
                }));
            }
            Message::LoadFindGameMatched(mut path) => {
                path.pop();
                let (modal, mut rec) = AppModal::async_dialog(
                    AsyncDialogKey::GameFind,
                    "Found Game Path",
                    format!(
                        "Program found the following path:\n\n{}\n\nIs this the correct one?",
                        path.display()
                    ),
                    vec!["No".to_string(), "Yes".to_string()],
                    AsyncDialogStrategy::Queue,
                );

                self.modal_stack.push(modal);

                return Task::future(async move {
                    if Some(1) == rec.next().await {
                        Message::LoadFindGameResolved(path)
                    } else {
                        Message::None
                    }
                });
            }
            Message::LoadFindGameResolved(path) => {
                self.save.game_directory = Some(path);
                if let Some(handle) = self.abortable_handles.remove(&AppAbortKey::GameFind) {
                    handle.abort();
                }
                return Task::done(Message::SetBusy(false));
            }
            // Possible TODO - convert this into a macro
            Message::LoadFindLocalRequested => {
                let (task, abort) = files::find_directories_matching(
                    "Documents/My Games/XCOM2 War of the Chosen/XComGame",
                );

                if let Some(handle) = self.abortable_handles.insert(AppAbortKey::LocalFind, abort) {
                    handle.abort();
                }

                return Task::done(Message::SetBusy(true)).chain(task.map(|p| match p {
                    Some(path) => Message::LoadFindLocalMatched(path),
                    None => Message::Chained(vec![
                        Message::AbortTask(AppAbortKey::LocalFind),
                        Message::SetBusy(false),
                    ]),
                }));
            }
            Message::LoadFindLocalMatched(path) => {
                let (modal, mut rec) = AppModal::async_dialog(
                    AsyncDialogKey::GameFind,
                    "Found Local Path",
                    format!(
                        "Program found the following path:\n\n{}\n\nIs this the correct one?",
                        path.display()
                    ),
                    vec!["No".to_string(), "Yes".to_string()],
                    AsyncDialogStrategy::Queue,
                );

                self.modal_stack.push(modal);

                return Task::future(async move {
                    if Some(1) == rec.next().await {
                        Message::LoadFindLocalResolved(path)
                    } else {
                        Message::None
                    }
                });
            }
            Message::LoadFindLocalResolved(path) => {
                self.save.local_directory = Some(path);
                if let Some(handle) = self.abortable_handles.remove(&AppAbortKey::LocalFind) {
                    handle.abort();
                }
                return Task::done(Message::SetBusy(false));
            }

            Message::ItemDetailsAddToLibraryRequest(ids) => {
                self.modal_stack.push(AppModal::AddToProfileRequest(ids));
            }
            Message::ItemDetailsAddToLibraryAllRequest(id) => {
                let client = Steam::new(self.api_key.expose_secret());
                let cache = self.file_cache.clone();
                return Task::done(Message::SetBusyMessage(
                    "Resolving dependencies...".to_string(),
                ))
                .chain(Task::future(async move {
                    match resolve_all_dependencies(id, client, cache).await {
                        Ok(dependencies) => Message::ItemDetailsAddToLibraryRequest(
                            dependencies.iter().copied().collect(),
                        ),
                        Err(err) => Message::display_error(
                            "Error Resolving Dependencies",
                            format!("An error occurred while resolving dependencies:\n{err:?}"),
                        ),
                    }
                }))
                .chain(Task::done(Message::SetBusy(false)));
            }

            #[cfg(target_os = "linux")]
            Message::DbusError(err) => {
                eprintln!("Unrecoverable dbus error: {err:?}");
                return iced::window::get_oldest().and_then(iced::window::close);
            }

            Message::GainFocus => {
                return iced::window::get_oldest().and_then(|id| {
                    Task::batch([
                        // Currently, winit does not implement window::focus_window on wayland;
                        // as such, additionally requesting user attention as a fallback
                        iced::window::request_user_attention(
                            id,
                            Some(iced::window::UserAttention::Informational),
                        ),
                        iced::window::gain_focus(id),
                    ])
                });
            }
            Message::CloseAppRequested => {
                if self.settings.steamcmd_save_password {
                    if !self.steamcmd_state.password.expose_secret().is_empty()
                        && let Err(err) = self
                        .credentials
                        .steam_password
                        .set_password(self.steamcmd_state.password.expose_secret())
                    {
                        eprintln!("Error saving password entry: {err:?}");
                    }
                } else if let Err(err) = self.credentials.steam_password.delete_credential() {
                    eprintln!("Error deleting password entry: {err:?}");
                }

                if self.settings.steam_webapi_save_api_key {
                    if !self.api_key.expose_secret().is_empty()
                        && let Err(err) = self
                        .credentials
                        .steam_web_api
                        .set_password(self.api_key.expose_secret())
                    {
                        eprintln!("Error saving web api key entry: {err:?}");
                    }
                } else if let Err(err) = self.credentials.steam_web_api.delete_credential() {
                    eprintln!("Error deleting api key entry: {err:?}");
                }

                if self.settings.steamcmd_logout_on_exit {
                    self.steamcmd_state.logout();
                }

                let result: Result<()> = try {
                    let file = std::fs::File::create(&*SAVE_PATH)?;
                    serde_json::to_writer_pretty(file, &self.save)?;
                };
                if let Err(err) = result {
                    eprintln!("Error saving application data: {err:?}");
                }

                return iced::window::get_oldest().and_then(iced::window::close);
            }
            Message::LoggingSetup(sender) => {
                println!("Logging should be set up...");
                let _ = Arc::get_mut(&mut self.steamcmd_state)
                    .expect(
                        "application should currently have exclusive ownership of steamcmd state",
                    )
                    .log_sender
                    .insert(sender);
            }
            Message::LogAppend(message) => {
                self.steamcmd_log
                    .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                self.steamcmd_log
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(strip_ansi_escapes::strip(message).as_bstr().to_string()),
                    )));
                self.steamcmd_log
                    .perform(text_editor::Action::Edit(text_editor::Edit::Enter));
            }
            Message::LogAction(action) => match action {
                text_editor::Action::Edit(_) => (),
                action => self.steamcmd_log.perform(action),
            },

            Message::ImageLoaded(id, handle) => {
                let _ = self.images.insert(id, handle);
            }

            Message::LaunchLogAction(action) => match action {
                text_editor::Action::Edit(_) => (),
                action => self.launch_log.perform(action),
            },
            Message::LaunchLogCreated(contents) => {
                self.launch_log.perform(text_editor::Action::SelectAll);
                self.launch_log
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(contents),
                    )));
                self.launch_log
                    .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
            }
            Message::LaunchLogAppended(contents) => {
                self.launch_log
                    .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                self.launch_log
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(contents),
                    )));
            }
            Message::LaunchLogClear => {
                self.launch_log = text_editor::Content::new();
            }

            Message::None => (),
        }

        Task::none()
    }

    fn update_settings(&mut self, message: SettingsMessage) -> Task<Message> {
        macro_rules! apply {
            ($name:ident,$ty:ty,$val:expr) => {
                *self
                    .settings_editing
                    .get_field_mut::<$ty>($name)
                    .expect(&format!(
                        "setting {} should be a {}",
                        $name,
                        stringify!($ty)
                    )) = $val
            };
        }

        match message {
            SettingsMessage::Edit(edit) => match edit {
                AppSettingEdit::String(name, new) | AppSettingEdit::Secret(name, new) => {
                    apply!(name, String, new)
                }
                AppSettingEdit::Bool(name, new) => apply!(name, bool, new),
                AppSettingEdit::Number(name, new) => {
                    let Ok(parsed) = new.parse::<u32>() else {
                        return Task::none();
                    };

                    let range = APP_SETTINGS_INFO
                        .field(name)
                        .and_then(|field| field.get_attribute::<AppSettingsUnsignedRange>())
                        .map(AppSettingsUnsignedRange::to_owned)
                        .unwrap_or_default();

                    if (range.min..=range.max).contains(&parsed) {
                        apply!(name, u32, parsed);
                    }
                }
            },
            SettingsMessage::ResetToDefault => self.settings_editing = AppSettings::default(),
            SettingsMessage::ResetToSaved => self.settings_editing = self.settings.clone(),
            SettingsMessage::Save => {
                let old = self.settings.clone();
                self.settings = self.settings_editing.clone();

                if old.download_directory != self.settings.download_directory {
                    self.scan_downloads();
                }

                if let Some(state) = Arc::get_mut(&mut self.steamcmd_state) {
                    state.command_path = self
                        .settings
                        .steamcmd_command_path
                        .is_empty()
                        .not()
                        .then_some(self.settings.steamcmd_command_path.clone().into());
                    state.download_dir = PathBuf::from(&self.settings.download_directory);
                } else {
                    self.modal_stack.push(AppModal::ErrorMessage(
                        "Warning".to_string(),
                        "Was unable to apply settings for steamcmd.\nPlease report this issue to the devs.".to_string(),
                    ));
                }

                let result: Result<()> = try {
                    std::fs::write(
                        &*SETTINGS_PATH,
                        serde_json::to_string_pretty(&self.settings)?,
                    )?
                };
                if let Err(err) = result {
                    eprintln!("Error saving settings: {err}");
                    return Task::done(Message::DisplayError(
                        "Error Saving Settings to Disk".to_string(),
                        err.to_string(),
                    ));
                };
            }
            SettingsMessage::None => (),
        }

        Task::none()
    }

    fn file_watch_event(&mut self, event: notify::Event) -> Task<Message> {
        match event.kind {
            // Prefer checking when download completes instead
            notify::EventKind::Create(notify::event::CreateKind::Folder) => {}
            notify::EventKind::Remove(notify::event::RemoveKind::Folder) => {}
            _ => (),
        };

        Task::none()
    }

    fn view_profile<'a>(&'a self, profile: &'a library::Profile) -> Element<'a, Message> {
        column![
            text(&profile.name).size(24),
            container(
                container(
                    column![
                        row![
                            text("Mods").size(20),
                            horizontal_space(),
                            button("View Save").on_press(Message::ProfileViewSaveRequested(profile.id)),
                            tooltip!(
                                button("Import Save").style(button::danger).on_press(Message::ProfileImportSaveRequested(profile.id)),
                                "Note that this will overwrite any save data currently in the profile.",
                            ),
                            // TODO - add confirmation
                            button("Delete Profile")
                                .style(button::danger)
                                .on_press(Message::ProfileDeletePressed(profile.id)),
                        ],
                        scrollable(column(profile.items.keys().map(|id| {
                            let mut issues = vec![];
                            if !self.item_downloaded(*id) {
                                issues.push("Missing: Mod is not downloaded (or found)".to_string());
                            }

                            issues.extend(
                                profile.compatibility_issues.get(id).map(Vec::as_slice).unwrap_or_default().iter().map(|item| {
                                    match item {
                                        library::CompatibilityIssue::MissingWorkshop(_id, message) => {
                                            format!("Missing Workshop Dependency: {message}")
                                        }
                                        library::CompatibilityIssue::MissingRequired(dlc_name) => {
                                            format!("Missing Dependency: {dlc_name}")
                                        },
                                        library::CompatibilityIssue::Incompatible(dlc_name) => {
                                            format!("Incompatible Mod: {dlc_name}")
                                        },
                                        library::CompatibilityIssue::Overlapping(name, provides) => {
                                            format!("{name} Provides Same DLCNames: {}", provides.iter().join(", "))
                                        },
                                        library::CompatibilityIssue::Unknown => {
                                            "Missing Info".to_string()
                                        },
                                    }
                                }
                            ));

                            let button_style = if let Some(sel_id) = profile.view_selected_item
                                && sel_id == *id
                            {
                                button::secondary
                            } else if !issues.is_empty() {
                                button::danger
                            } else {
                                button::primary
                            };
                            let missing_text = if self.item_downloaded(*id) {
                                ""
                            } else {
                                " (MISSING)"
                            };
                            let button = if let Some(details) = self.file_cache.get_details(*id) {
                                button(text!("{} ({id}){missing_text}", details.title))
                                    .on_press(Message::ProfileItemSelected(*id))
                            } else {
                                button(text!("UNKNOWN ({id}){missing_text}"))
                            }
                            .style(button_style)
                            .width(Fill);

                            if !issues.is_empty() {
                                tooltip!(
                                    button,
                                    column(issues.into_iter().map(|s| text(s).into())),
                                    tooltip::Position::Bottom,
                                ).into()
                            } else {
                                button.into()
                            }
                        })))
                        .height(128),
                    ]
                    .push(profile.view_selected_item.map(|item_id| {
                        row![
                            button("View Details").on_press(Message::SetViewingItem(item_id)),
                            horizontal_space(),
                            button("Remove Mod")
                                .style(button::danger)
                                .on_press(Message::ProfileRemoveItems(profile.id, vec![item_id]))
                        ]
                    }))
                    .push(
                        profile
                            .view_selected_item
                            .is_some()
                            .then(|| self.mod_editor.view(self))
                    )
                )
                .width(Fill)
                .padding(8)
                .style(container::dark)
            )
            .padding(16),
        ]
        .into()
    }

    fn view_item_detailed(&self, id: u32) -> Element<'_, Message> {
        let Some(file) = self.file_cache.get_details(id) else {
            return column![
                text("Failed to load details"),
                vertical_space(),
                row![
                    horizontal_space(),
                    button("Close")
                        .style(button::danger)
                        .on_press(Message::CloseModal)
                ],
            ]
            .height(Fill)
            .width(Fill)
            .into();
        };

        let download_button = if self.item_downloaded(id) {
            button("Update")
        } else {
            button("Download")
        }
        .on_press_maybe(
            self.is_downloading(id)
                .not()
                .then_some(Message::SteamCMDDownloadRequested(id)),
        );

        fn handle_url(url: reqwest::Url) -> Message {
            if url.domain() == Some("steamcommunity.com")
                && url
                    .path_segments()
                    .is_some_and(|mut split| split.nth_back(1) == Some("filedetails"))
                && let Some(id) = url
                    .query_pairs()
                    .find_map(|(name, value)| name.eq("id").then(|| value.parse::<u32>().ok()))
                    .flatten()
            {
                Message::SetViewingItem(id)
            } else {
                std::thread::spawn(move || {
                    if let rfd::MessageDialogResult::Yes = rfd::MessageDialog::new()
                        .set_title("Warning: External Link")
                        .set_description(format!(
                            "This link leads to:\n\n{url}\n\n are you sure you want to go there?"
                        ))
                        .set_buttons(rfd::MessageButtons::YesNo)
                        .show()
                    {
                        let _ = opener::open(url.as_str());
                    }
                });
                Message::None
            }
        }

        container(
            container(column![
                row![
                    web::image_maybe(&self.images, file.preview_url.clone())
                        .height(256)
                        .width(256)
                        .padding(16),
                    column![
                        text(file.title.clone()).size(18),
                        text!("{:.2} out of 10", file.get_score() * 10.0),
                    ]
                    .push(file.children.is_empty().not().then(|| {
                        row!["Dependencies -"]
                            .extend(file.children.iter().map(|child| {
                                let child_id = child
                                    .published_file_id
                                    .parse::<u32>()
                                    .expect("id should be a valid id");
                                if let Some(details) = self.file_cache.get_details(child_id) {
                                    row![
                                        rich_text([span(format!(
                                            "{} ({child_id}) ",
                                            details.title
                                        ))
                                        .link(child_id)])
                                        .on_link_click(Message::SetViewingItem),
                                        if self.item_downloaded(child_id) {
                                            symbols::check()
                                        } else {
                                            symbols::xmark()
                                        },
                                    ]
                                } else {
                                    row![text!("UNKNOWN ({child_id})")]
                                }
                                .into()
                            }))
                            .spacing(8)
                            .wrap()
                    }))
                    .push(row![
                        download_button,
                        button("Add to Profile")
                            .on_press(Message::ItemDetailsAddToLibraryRequest(vec![id])),
                    ])
                    .push(row![
                        file.children.is_empty().not().then_some(
                            button("Download All Dependencies")
                                .on_press(Message::DownloadAllRequested(id))
                        ),
                        button("Add All Dependencies to Profile")
                            .on_press(Message::ItemDetailsAddToLibraryAllRequest(id))
                    ])
                    .push(
                        self.item_downloaded(id).then_some(
                            button("Delete")
                                .style(button::danger)
                                .on_press(Message::DeleteRequested(id))
                        )
                    ),
                ],
                column![
                    text("Description"),
                    scrollable(
                        container(
                            markdown(
                                self.markup_cache
                                    .get_markup(file.get_description())
                                    .unwrap_or_default(),
                                markdown::Settings::with_style(markdown::Style::from_palette(
                                    self.theme().palette()
                                )),
                            )
                            .map(handle_url),
                        )
                        .padding(8)
                        .style(container::dark)
                    )
                    .height(Fill),
                    row![
                        button("Back")
                            .style(button::danger)
                            .on_press(Message::CloseModal),
                    ]
                ]
                .spacing(4)
                .padding(16),
            ])
            .style(container::dark)
            .style(container::rounded_box)
            .padding(16),
        )
        .center(Fill)
        .width(Fill)
        .padding(32)
        .into()
    }

    fn async_dialog_modal<'a>(&'a self, dialog: &'a AppModal) -> Element<'a, Message> {
        let AppModal::AsyncDialog {
            key,
            title,
            body,
            sender: _,
            options,
            strategy: _,
        } = dialog
        else {
            panic!("async_dialog_modal should only be passed an async dialog")
        };

        iced_aw::card(
            text(title),
            column![
                text(body),
                row(options.iter().enumerate().map(|(i, display)| {
                    button(text(display))
                        .on_press(Message::AsyncDialogResolve(*key, i))
                        .into()
                }))
            ],
        )
        .width(Shrink)
        .into()
    }

    fn settings_page(&self) -> Element<'_, Message> {
        let settings = &self.settings_editing;
        let col = column(APP_SETTINGS_INFO.iter().map(|field| {
            let name = field.name();

            let display = field
                .get_attribute::<AppSettingsLabel>()
                .expect("label should be set");
            let description = field.get_attribute::<AppSettingsDescription>();
            let label = tooltip(
                row![text(display.0).align_y(Vertical::Center).height(Fill),]
                    .push(description.is_some().then_some(text("?").size(12))),
                match description {
                    Some(description) => container(text(description.0))
                        .style(container::rounded_box)
                        .padding(16),
                    None => container(""),
                },
                tooltip::Position::Bottom,
            )
            .into();

            let editor = match field
                .get_attribute::<AppSettingEditor>()
                .map(AppSettingEditor::to_owned)
                .unwrap_or_else(|| {
                    AppSettingEditor::get_default_for(
                        field
                            .type_info()
                            .expect("field should not be a dynamic type"),
                    )
                }) {
                AppSettingEditor::StringInput => text_input(
                    AppSettings::default()
                        .get_field::<String>(name)
                        .expect("fields should have a default value"),
                    settings
                        .get_field::<String>(name)
                        .expect("string input should only be set for string values"),
                )
                .width(600)
                .on_input(|new| AppSettingEdit::String(name, new).into())
                .into(),
                AppSettingEditor::SecretInput => text_input(
                    "unset",
                    settings
                        .get_field::<String>(name)
                        .expect("secret input should only be set for string values"),
                )
                .secure(true)
                .width(600)
                .on_input(|new| AppSettingEdit::Secret(name, new).into())
                .into(),
                AppSettingEditor::BoolToggle => container(
                    widget::toggler(
                        *settings
                            .get_field::<bool>(name)
                            .expect("bool toggle should only be set for bool values"),
                    )
                    .on_toggle(|toggled| AppSettingEdit::Bool(name, toggled).into()),
                )
                .center_y(Fill)
                .into(),
                AppSettingEditor::NumberInput => {
                    let value = settings
                        .get_field::<u32>(name)
                        .expect("number input should only be set for u32 values");
                    let editor = text_input(
                        AppSettings::default()
                            .get_field::<u32>(name)
                            .expect("number input should only be set for u32 values")
                            .to_string()
                            .as_str(),
                        value.to_string().as_str(),
                    )
                    .width(200)
                    .on_input(|new| AppSettingEdit::Number(name, new).into());

                    if field.get_attribute::<AppSettingsTimePreview>().is_some() {
                        let preview =
                            humantime::format_duration(Duration::from_secs(*value as u64));
                        row![text!("({preview}) ").height(Fill).center(), editor,].into()
                    } else {
                        editor.into()
                    }
                }
            };

            row([label, horizontal_space().into(), editor])
                .height(32)
                .into()
        }));

        let col = col.push(vertical_space());
        col.push(row![
            horizontal_space(),
            button("Reset to Default").on_press(SettingsMessage::ResetToDefault.into()),
            button("Reset to Saved").on_press(SettingsMessage::ResetToSaved.into()),
            button("Save").on_press_maybe(
                (self.settings != self.settings_editing).then_some(SettingsMessage::Save.into())
            )
        ])
        .padding(16)
        .into()
    }

    fn view(&self) -> Element<'_, Message> {
        modal(
            !self.modal_stack.is_empty(),
            column![
                row(AppPage::iter().map(|page| {
                    button(text(page.to_string()))
                        .on_press(Message::SetPage(page))
                        .style(if page == self.current_page {
                            button::secondary
                        } else {
                            button::primary
                        })
                        .into()
                })),
                match self.current_page {
                    AppPage::Main => self.main_page(),
                    AppPage::Library => self.library_page(),
                    AppPage::Profiles => self.profiles_page(),
                    AppPage::Browse => self.browse_page(),
                    AppPage::SteamCMD => self.steamcmd_page(),
                    AppPage::Settings => self.settings_page(),
                    AppPage::Downloads => self.downloads_page(),
                    AppPage::GameLogs => self.game_logs_page(),
                }
            ],
            {
                self.modal_stack.iter().filter_map(|modal| {
                    match modal {
                        AppModal::ApiKeyRequest => self.steam_api_key_modal(),
                        AppModal::SteamGuardCodeRequest => self.steam_guard_code_request_mdodal(),
                        AppModal::ErrorMessage(title, message) => {
                            self.error_message_modal(title, message)
                        }
                        AppModal::LibraryDeleteRequest => self.library_delete_request_modal(),
                        AppModal::ProfileAddRequest => self.profile_add_modal(),
                        AppModal::AddToProfileRequest(ids) => self.add_to_profile_modal(ids),
                        AppModal::ItemDetailedView(id) => self.view_item_detailed(*id),
                        dialog @ AppModal::AsyncDialog {
                            strategy: AsyncDialogStrategy::Stack | AsyncDialogStrategy::Replace,
                            ..
                        } => self.async_dialog_modal(dialog),
                        dialog @ AppModal::AsyncDialog {
                            strategy: AsyncDialogStrategy::Queue,
                            key: current_key,
                            ..
                        } => {
                            if self
                                .modal_stack
                                .iter()
                                .take_while(|other| other != &modal)
                                .any(|other| {
                                    matches!(other, AppModal::AsyncDialog { key, .. } if key == current_key)
                                })
                            {
                                return None;
                            }

                            self.async_dialog_modal(dialog)
                        }

                        AppModal::Busy => self.busy_modal(),
                        AppModal::BusyMessage(msg) => self.busy_message_modal(msg),
                    }.apply(Some)
                })
            },
        )
        .into()
    }

    fn main_page(&self) -> Element<'_, Message> {
        centered(
            column![
                LabeledFrame::new(
                    "Active Profile",
                    combo_box(
                        &self.active_profile_combo,
                        "Select Profile...",
                        self.save.active_profile.and_then(|id| self
                            .save
                            .profiles
                            .iter()
                            .find_map(|(i, p)| (*i == id).then_some(&p.name))),
                        Message::ActiveProfileSelected,
                    )
                ),
                LabeledFrame::new(
                    "Game Directory",
                    row![
                        button(symbols::folder()).on_press(Message::LoadPickGameDirectory),
                        tooltip!(
                            button(symbols::magnifying_glass()).on_press(Message::LoadFindGameRequested),
                            "Automatically find game directory",
                        ),
                        text_input(
                            "/path/to/XCom2-WarOfTheChosen",
                            &self
                                .save
                                .game_directory
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_default()
                        )
                        .on_input(|s| Message::LoadSetGameDirectory(PathBuf::from(s))),
                    ],
                ),
                LabeledFrame::new("Local Directory",
                    row![
                        button(symbols::folder()).on_press(Message::LoadPickLocalDirectory),
                        tooltip!(
                            button(symbols::magnifying_glass()).on_press(Message::LoadFindLocalRequested),
                            "Automatically find. Note this is likely to fail if you haven't launched the game at least once before.",
                        ),
                        tooltip!(
                            text_input(
                                "/path/to/Documents/My Games/XCOM2 War of the Chosen/XComGame",
                                &self
                                    .save
                                    .local_directory
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default()
                            ).on_input(|s| Message::LoadSetLocalDirectory(PathBuf::from(s))),
                            container("This is the path where the game expects find your local data (e.g. saves). Files/folders that are modified will have backups automatically created."),
                        ),
                    ],
                ),
                LabeledFrame::new(
                    "Launch Command",
                    row![
                        button(symbols::folder()).on_press(Message::LoadPickLaunchCommand),
                        text_input(
                            "ex: /usr/bin/xdg-open",
                            self.save.launch_command.as_deref().unwrap_or_default(),
                        )
                        .on_input(Message::LoadSetLaunchCommand),
                    ],
                ),
                LabeledFrame::new(
                    "Launch Args",
                    column(self.save.launch_args.iter().enumerate().map(|(i, (l, r))| {
                        row![
                            text_input("key (optional)", l)
                                .on_input(move |k| Message::LoadEditArgKey(i, k)),
                            text_input("value (required)", r)
                                .on_input(move |v| Message::LoadEditArgValue(i, v)),
                            button(symbols::xmark())
                                .style(button::danger)
                                .on_press(Message::LoadRemoveLaunchArgs(i))
                                .width(Shrink)
                        ]
                        .into()
                    }))
                    .width(Fill)
                    .push(button("Add +").on_press(Message::LoadAddLaunchArgs))
                ),
                row![
                    button("Build").on_press(Message::LoadPrepareProfile(true)),
                    button("Launch").on_press(Message::LoadLaunchGame),
                    // button("Text Check Update").on_press(Message::SteamCMDCheckUpdateRequested)
                ].spacing(8),
            ]
            .padding(16),
        )
    }

    fn library_page(&self) -> Element<'_, Message> {
        let mut grid = iced_aw::grid!()
            .column_spacing(8)
            .row_height(Shrink)
            .row_spacing(8);

        let all_selected = self.library.iter_filtered().all(|item| item.selected);
        let some_selected = self.library.iter_filtered().any(|item| item.selected);

        grid = grid.push(iced_aw::grid_row!(
            checkbox("", all_selected).on_toggle(Message::LibraryToggleAll),
            text(""),
            text("Workshop ID"),
            text("Title"),
            text("ID"),
            text("Path"),
        ));

        for item in self.library.iter_filtered() {
            let id = item.id;
            let missing = self.library.missing_dependencies.get(&id);
            let text_style = if missing.is_some() {
                text::danger
            } else {
                text::default
            };
            grid = grid.push(iced_aw::grid_row!(
                checkbox("", item.selected)
                    .on_toggle(move |toggle| Message::LibraryToggleItem(id, toggle)),
                button(symbols::eye()).on_press(Message::SetViewingItem(id)),
                tooltip(
                    rich_text([span(id).link(id.to_string()).underline(missing.is_some())])
                        .on_link_click(|link: String| {
                            println!("link {link} was clicked");
                            Message::None
                        })
                        .style(text_style),
                    if let Some(missing) = missing {
                        container(column(
                            missing.iter().map(|s| text!("Missing Item: {s}").into()),
                        ))
                        .padding(16)
                        .style(container::bordered_box)
                    } else {
                        container("")
                    },
                    tooltip::Position::Right,
                ),
                text(&item.title).style(text_style),
                text(
                    self.metadata
                        .get(&id)
                        .map(|data| data.dlc_name.as_str())
                        .unwrap_or("UNKNOWN")
                )
                .style(text_style),
                text(item.path.display().to_string()).style(text_style),
            ));
        }

        column![
            row![
                row![button("Rescan").on_press(Message::LibraryScanRequest)],
                vertical_rule(2),
                row![
                    button("Update")
                        .on_press_maybe(some_selected.then_some(Message::LibraryUpdateRequest)),
                    button("Add to Profile").on_press_maybe(
                        some_selected.then_some(Message::LibraryAddToProfileRequest)
                    ),
                    button("Delete")
                        .style(button::danger)
                        .on_press_maybe(some_selected.then_some(Message::LibraryDeleteRequest)),
                ],
            ]
            .spacing(6)
            .height(32),
            row![
                tooltip!(
                    toggler(self.library.filter_fuzzy).on_toggle(Message::LibraryFilterToggleFuzzy),
                    "Fuzzy Matching",
                    tooltip::Position::Bottom,
                ),
                text_input("Filter", &self.library.filter_query)
                    .on_input(Message::LibraryFilterUpdateQuery),
            ],
            container(
                scrollable(container(grid).padding(16))
                    .direction(scrollable::Direction::Both {
                        vertical: Default::default(),
                        horizontal: Default::default()
                    })
                    .width(Fill)
                    .height(Fill)
            ),
        ]
        .width(Fill)
        .height(Fill)
        .into()
    }

    fn profiles_page(&self) -> Element<'_, Message> {
        macro_rules! sel_button {
            ($inner:expr) => {
                button(text($inner).height(Fill).width(Fill).size(14)).height(30)
            };
        }

        let mut select_col = iced_aw::grid![].column_width(Fill).width(256);

        select_col = select_col.extend(self.save.profiles.values().map(|profile| {
            let style = if self.selected_profile_id.is_some_and(|id| id == profile.id) {
                button::secondary
            } else {
                button::primary
            };

            iced_aw::grid_row![
                sel_button!(profile.name.as_str())
                    .style(style)
                    .on_press(Message::ProfileSelected(profile.id))
            ]
        }));

        select_col = select_col.push(iced_aw::grid_row![
            sel_button!("Add Profile +").on_press(Message::ProfileAddPressed)
        ]);

        let profile_col = if let Some(id) = self.selected_profile_id
            && let Some(profile) = self.save.profiles.get(&id)
        {
            // container(column![text(profile.name.as_str()).size(32)])
            container(self.view_profile(profile))
        } else {
            container("No Profile Selected...")
        };

        row![select_col, profile_col]
            .height(Fill)
            .width(Fill)
            .into()
    }

    fn browse_page(&self) -> Element<'_, Message> {
        let items = row(self.loaded_files.iter().map(|file| {
            let id = &file
                .published_file_id
                .parse::<u32>()
                .expect("id should be a valid u32");
            container(
                column![
                    web::image_maybe(&self.images, &file.preview_url)
                        .width(Fill)
                        .height(300),
                    text(&file.title),
                    text(&file.published_file_id),
                    text!("{:.2} out of 10", file.get_score() * 10.0),
                    button(text("View").align_x(Center))
                        .width(Fill)
                        .on_press(Message::SetViewingItem(*id)),
                    button(
                        text(if self.item_downloaded(*id) {
                            "Update"
                        } else {
                            "Download"
                        })
                        .align_x(Center)
                    )
                    .width(Fill)
                    .on_press_maybe(
                        self.is_downloading(*id)
                            .not()
                            .then_some(Message::SteamCMDDownloadRequested(*id))
                    ),
                    text({
                        file.get_description()
                            .char_indices()
                            .take(64)
                            .map(|(_, ch)| ch)
                            .collect::<String>()
                    }),
                ]
                .spacing(4)
                .height(Shrink)
                .width(Fill),
            )
            .style(container::secondary)
            .padding(16)
            .max_width(300)
            .into()
        }))
        .spacing(16)
        .padding(16)
        .height(Shrink)
        .wrap();

        column![
            row![
                text("API Key"),
                text_input("API Key...", self.api_key.expose_secret())
                    .on_input(Message::SetApiKey)
                    .secure(true)
            ],
            row![
                text("Search"),
                text_input("Query...", &self.browse_query_edit)
                    .on_input(Message::BrowseEditQuery)
                    .on_submit(Message::BrowseSubmitQuery)
            ],
            row![pick_list(
                web::WorkshopSort::all_with_period(self.browsing_query_period),
                Some(self.browsing_query.sort),
                Message::BrowseEditSort
            )]
            .push(match self.browsing_query.sort {
                web::WorkshopSort::Trend(period) => {
                    Some(pick_list(
                        web::WorkshopTrendPeriod::VARIANTS,
                        Some(period),
                        Message::BrowseEditPeriod,
                    ))
                }
                _ => None,
            })
            .push(
                iced_aw::DropDown::new(
                    button("Tags").width(256).style(button::secondary).on_press(
                        Message::BrowseToggleTagsDropdown(!self.browse_query_tags_open)
                    ),
                    container(column(web::XCOM2WorkshopTag::VARIANTS.iter().map(|tag| {
                        row![
                            text(tag.to_string()),
                            horizontal_space(),
                            toggler(self.browsing_query.tags.contains(tag))
                                .on_toggle(|_| Message::BrowseEditTag(*tag))
                        ]
                        .into()
                    })),)
                    .style(container::dark)
                    .padding(8),
                    self.browse_query_tags_open,
                )
                .on_dismiss(Message::BrowseToggleTagsDropdown(false))
            ),
            scrollable(column!(items))
                .height(iced::Length::Fill)
                .anchor_top(),
            row![
                button("<").on_press_maybe(
                    (self.browsing_page_max > 0 && self.browsing_page > 1)
                        .then(|| Message::SetBrowsePage(self.browsing_page - 1))
                ),
                horizontal_space(),
                container(if self.browsing_page_max > 0 {
                    text!("Page: {} of {}", self.browsing_page, self.browsing_page_max)
                } else {
                    text!("Nothing to see...")
                })
                .padding(4),
                horizontal_space(),
                button(">").on_press_maybe(
                    (self.browsing_page_max > 0 && self.browsing_page < self.browsing_page_max)
                        .then(|| Message::SetBrowsePage(self.browsing_page + 1))
                ),
            ]
            .height(32)
        ]
        .into()
    }

    fn steamcmd_page(&self) -> Element<'_, Message> {
        let state = &self.steamcmd_state;
        let settings = &self.steamcmd_state;
        let can_log_in = !(state.logged_in
            || settings.username.is_empty()
            || settings.password.expose_secret().is_empty());
        column![
            row![
                text("Username"),
                text_input("User", &self.steamcmd_state.username)
                    .on_input(Message::SetSteamCMDUser)
                    .on_submit_maybe(can_log_in.then_some(Message::SteamCMDLogin(true))),
                text("Password"),
                text_input("Password", self.steamcmd_state.password.expose_secret())
                    .secure(true)
                    .on_input(Message::SetSteamCMDPassword)
                    .on_submit_maybe(can_log_in.then_some(Message::SteamCMDLogin(true))),
                button("Log In").on_press_maybe(can_log_in.then_some(Message::SteamCMDLogin(true))),
            ],
            container(
                text_editor(&self.steamcmd_log)
                    .placeholder("Log currently empty...")
                    .on_action(Message::LogAction)
                    .font(iced::Font::MONOSPACE)
                    .highlight("log", iced::highlighter::Theme::Leet)
                    .height(Fill)
            )
            .style(container::dark)
        ]
        .into()
    }

    fn busy_modal(&self) -> Element<'_, Message> {
        container(iced_aw::spinner::Spinner::new().height(32).width(32))
            .center(Fill)
            .into()
    }

    fn busy_message_modal<'a>(&'a self, msg: &'a str) -> Element<'a, Message> {
        container(
            column![
                text(msg),
                iced_aw::spinner::Spinner::new().height(32).width(32)
            ]
            .spacing(8)
            .align_x(Center),
        )
        .style(container::rounded_box)
        .padding(16)
        .into()
    }

    fn steam_api_key_modal(&self) -> Element<'_, Message> {
        container(
            container(column![
                text(
                    "Note: This app requires a Steam API Key to function. Please enter your key."
                ),
                text("(Enable 'Save API Key' in settings if you would like to avoid doing this every time.)"),
                text("API Key"),
                text_input("API Key...", self.api_key.expose_secret())
                    .on_input(Message::ApiKeyRequestUpdate)
                    .secure(true)
                    .on_submit(Message::ApiKeySubmit),
                button("Submit").on_press_maybe(self.api_key.expose_secret().is_empty().not().then_some(Message::ApiKeySubmit)),
            ])
            .height(200)
            .width(500)
            .padding(16)
            .style(|theme: &Theme| container::Style {
                text_color: Some(theme.palette().primary.inverse()),
                background: Some(iced::Background::Color(theme.palette().primary)),
                border: iced::border::rounded(10),
                ..Default::default()
            }),
        )
        .center(Fill)
        .into()
    }

    fn steam_guard_code_request_mdodal(&self) -> Element<'_, Message> {
        centered(modal_box(
            200,
            300,
            column![
                text("Login Information Not Cached"),
                text("Please enter your Steam Guard code"),
                row![
                    text_input("Code", self.steamcmd_code.expose_secret())
                        .on_input(Message::SetSteamCMDCode)
                        .secure(true),
                    button("Submit").on_press_maybe(
                        self.steamcmd_code
                            .expose_secret()
                            .is_empty()
                            .not()
                            .then_some(Message::SteamCMDLoginElevatedSubmit)
                    )
                ],
                button("Cancel").on_press(Message::SteamCMDLoginElevatedCancel),
            ],
        ))
    }

    #[inline]
    fn is_downloading(&self, id: u32) -> bool {
        self.ongoing_downloads.contains_key(&id) || self.download_queue.contains_key(&id)
    }

    fn downloads_page(&self) -> Element<'_, Message> {
        let get_details = |(id, size): (&u32, &u64)| -> Element<'_, Message> {
            if let Some(details) = self.file_cache.get_details(*id) {
                let total_size = details.file_size.parse::<u64>().ok().unwrap_or_default();
                let percent = *size as f32 / total_size as f32 * 100.0;
                let total_display = files::SizeDisplay::automatic(total_size);
                let size_display = files::SizeDisplay::automatic_from(*size, total_size);
                row![
                    column![
                        text!(
                            "{} ({id}) - {size_display} of {total_display}",
                            details.title
                        ),
                        stack![
                            progress_bar(0.0..=total_size as f32, *size as f32),
                            container(text!("{percent:.2}%").align_x(Center).align_y(Center))
                                .center(Fill)
                        ],
                    ]
                    .width(Fill),
                    button("Cancel")
                        .style(button::danger)
                        .on_press(Message::DownloadCancelRequested(*id))
                ]
                .into()
            } else {
                let displayed_size = files::SizeDisplay::automatic(*size);
                row![
                    text!("Unknown ({id}) - {displayed_size}"),
                    horizontal_space(),
                    button("Cancel")
                        .style(button::danger)
                        .on_press(Message::DownloadCancelRequested(*id))
                ]
                .into()
            }
        };

        let mut col = column(None).spacing(8);

        let ongoing_empty = self.ongoing_downloads.is_empty();
        let queue_empty = self.download_queue.is_empty();
        let completed_empty = self.completed_downloads.is_empty();
        let errored_empty = self.errorred_downloads.is_empty();

        if !ongoing_empty {
            col = col.push(text("Ongoing Downloads").size(24));
            col = col.extend(self.ongoing_downloads.iter().map(get_details))
        }

        if !queue_empty {
            col = col.push(text("Queued Downloads").size(24));
            col = col.extend(self.download_queue.iter().map(get_details));
        }

        if !completed_empty {
            col = col.push(
                row![
                    text("Completed").size(24),
                    horizontal_space(),
                    button("Clear All").on_press(Message::SteamCMDDownloadCompletedClear(
                        self.completed_downloads.iter().copied().collect()
                    ))
                ]
                .align_y(Vertical::Bottom),
            );
            col = col.extend(
                self.completed_downloads
                    .iter()
                    .map(|id| -> Element<'_, Message> {
                        let info = if let Some(details) = self.file_cache.get_details(*id) {
                            format!("{} ({id})", details.title)
                        } else {
                            format!("Unknown ({id})")
                        };
                        row![
                            text(info),
                            horizontal_space(),
                            button("Clear")
                                .on_press(Message::SteamCMDDownloadCompletedClear(vec![*id])),
                        ]
                        .into()
                    }),
            )
        }

        if !errored_empty {
            col = col.push(
                row![
                    text("Errored Downloads").size(24),
                    horizontal_space(),
                    button("Clear All").on_press(Message::SteamCMDDownloadErrorClear(
                        self.errorred_downloads.keys().copied().collect()
                    ))
                ]
                .align_y(Vertical::Bottom),
            );
            col = col.extend(self.errorred_downloads.iter().map(
                |(id, reason)| -> Element<'_, Message> {
                    let info = if let Some(details) = self.file_cache.get_details(*id) {
                        format!("{} ({id})\n{reason}", details.title)
                    } else {
                        format!("Unknown ({id})\n{reason}")
                    };

                    row![
                        text(info),
                        horizontal_space(),
                        button("Retry").on_press(Message::SteamCMDDownloadRequested(*id)),
                        button("Clear").on_press(Message::SteamCMDDownloadErrorClear(vec![*id])),
                    ]
                    .into()
                },
            ));
        }

        if ongoing_empty && queue_empty && completed_empty && errored_empty {
            col = col.push(text("No ongoing downloads..."))
        }

        scrollable(col).into()
    }

    fn game_logs_page(&self) -> Element<'_, Message> {
        column![
            button("Clear").on_press(Message::LaunchLogClear),
            container(
                text_editor(&self.launch_log)
                    .placeholder("Log currently empty...")
                    .on_action(Message::LaunchLogAction)
                    .font(iced::Font::MONOSPACE)
                    .highlight("log", iced::highlighter::Theme::Leet)
                    .height(Fill),
            )
            .style(container::dark)
            .width(Fill)
        ]
        .into()
    }

    fn queue_downloads(&mut self) -> Task<Message> {
        if !self.steamcmd_state.logged_in {
            return Task::none();
        }

        let mut tasks = Vec::with_capacity(self.settings.simultaneous_downloads as usize);
        while self.ongoing_downloads.len() < self.settings.simultaneous_downloads as usize
            && let Some((id, _)) = self.download_queue.pop_front()
        {
            tasks.push(self.perform_download(id));
        }

        Task::batch(tasks)
    }

    fn perform_download(&mut self, id: u32) -> Task<Message> {
        self.ongoing_downloads.insert(id, 0);
        let state = self.steamcmd_state.clone();
        let retries = self.settings.automatic_download_retries;

        let (task, handle) = Task::perform(
            async move { state.download_item_with_retries(id, retries).await },
            move |res| match res {
                Ok(id) => Message::SteamCMDDownloadCompleted(id),
                Err(err) => Message::Chained(vec![
                    Message::DisplayError(format!("Error Downloading {id}"), err.to_string()),
                    Message::SteamCMDDownloadErrored(id, err.to_string()),
                    Message::SteamCMDDownloadCompleted(id),
                ]),
            },
        )
        .abortable();

        self.cancel_download_handles.insert(id, handle);

        task
    }

    fn profile_add_modal(&self) -> Element<'_, Message> {
        // TODO for all cards - fix border/header color not respecting theme
        iced_aw::card(
            "Add Profile",
            column![row![
                text("Name"),
                text_input("Profile Name", self.profile_add_name.as_str())
                    .on_input(Message::ProfileAddEdited)
                    .on_submit(Message::ProfileAddCompleted(true)),
            ]]
            .spacing(4),
        )
        .foot(row![
            button("Cancel")
                .style(button::danger)
                .on_press(Message::ProfileAddCompleted(false)),
            horizontal_space(),
            button("Confirm").style(button::success).on_press_maybe(
                self.profile_add_name
                    .is_empty()
                    .not()
                    .then_some(Message::ProfileAddCompleted(true))
            )
        ])
        .width(300)
        .into()
    }

    fn add_to_profile_modal<'a>(&'a self, items: &'a [u32]) -> Element<'a, Message> {
        let grid = iced_aw::grid![iced_aw::grid_row![
            checkbox(
                "",
                self.save
                    .profiles
                    .values()
                    .all(|profile| profile.add_selected)
            )
            .on_toggle(Message::LibraryAddToProfileToggleAll),
            text("Name")
        ]]
        .column_widths(&[Shrink, Shrink]);

        iced_aw::card(
            "Add to Profile",
            scrollable(grid.extend(self.save.profiles.iter().map(|(id, profile)| {
                iced_aw::grid_row![
                    checkbox("", profile.add_selected)
                        .on_toggle(|toggle| Message::LibraryAddToProfileToggled(*id, toggle)),
                    text(profile.name.as_str())
                ]
            }))),
        )
        .foot(row![
            button("Cancel")
                .style(button::danger)
                .on_press(Message::LibraryAddToProfileConfirm(Vec::new())),
            horizontal_space(),
            button("Confirm")
                .style(button::success)
                .on_press_with(|| Message::LibraryAddToProfileConfirm(items.to_vec())),
        ])
        .width(300)
        .into()
    }

    fn error_message_modal(
        &self,
        title: impl ToString,
        message: impl ToString,
    ) -> Element<'_, Message> {
        iced_aw::card(text(title.to_string()), column![text(message.to_string()),])
            .foot(row![
                horizontal_space(),
                button("Close")
                    .style(button::danger)
                    .on_press(Message::CloseModal)
            ])
            .width(320)
            .on_close(Message::CloseModal)
            .into()
    }

    fn library_delete_request_modal(&self) -> Element<'_, Message> {
        card(
            "Warning",
            column![
                text("Are you sure you want to delete the following items?"),
                scrollable(column(
                    self.library
                        .iter_selected()
                        .map(|item| text!("{} ({})", item.title, item.id).into())
                ))
            ],
        )
        .foot(row![
            button("Cancel").on_press(Message::CloseModal),
            horizontal_space(),
            button("Delete")
                .style(button::danger)
                .on_press(Message::LibraryDeleteConfirm),
        ])
        .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::Subscription::batch([
            #[cfg(target_os = "linux")]
            iced::advanced::subscription::from_recipe(ConnectionRecipe(
                self.dbus_connection.clone(),
            )),
            iced::Subscription::run(steamcmd::setup_logging),
            iced::Subscription::run(web::setup_background_resolver),
            // Need to consider how to handle the downloads directory not existing yet
            // iced::advanced::subscription::from_recipe(files::WatcherRecipe(
            //     files::get_all_items_directory(&self.settings.download_directory),
            // )),
            iced::window::close_requests().map(|_| Message::CloseAppRequested),
            iced::event::listen_with(|ev, _status, _id| match ev {
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::EscapeModal),
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Back,
                )) => Some(Message::EscapeModal),
                _ => None,
            }),
        ])
    }

    // Potential TODO: If this ends up being an expensive operation on large mod libraries,
    // add option to skip checking already included files or turn it into a stream
    fn scan_downloads(&mut self) {
        self.downloaded.clear();
        let dir = files::get_all_items_directory(&self.settings.download_directory);
        // Assume fresh state where nothing has been downloaded before
        if !dir.exists() {
            return;
        }

        match std::fs::read_dir(&dir) {
            Ok(read) => {
                for entry in read {
                    match entry {
                        Ok(entry) if entry.file_type().ok().is_some_and(|ty| ty.is_dir()) => {
                            let entry = entry.path();
                            let metadata_file = std::fs::read_dir(&entry).ok().and_then(|mut r| {
                                r.find_map(|res| {
                                    if let Ok(e) = res
                                        && e.path().extension().is_some_and(|ext| ext == "XComMod")
                                    {
                                        Some(e.path())
                                    } else {
                                        None
                                    }
                                })
                            });
                            if let Some(meta) = metadata_file
                                && let Some(name) = meta
                                    .with_extension("")
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                && let Ok(raw) = std::fs::read_to_string(&meta)
                            {
                                match xcom_mod::ModMetadata::deserialize_from_str(raw, name) {
                                    Ok(mut data) => {
                                        let file_name =
                                            entry.file_name().expect("folder should have a name");
                                        // Some define the value here instead of inside of their metadata file
                                        if let Ok(Ok(id)) = std::fs::read_to_string(
                                            entry.join("PublishedFileId.ID"),
                                        )
                                        .as_deref()
                                        .map(str::trim)
                                        .map(str::parse::<u32>)
                                        {
                                            data.published_file_id = id;
                                        }

                                        if *file_name != *data.published_file_id.to_string() {
                                            eprintln!(
                                                "Found a mod that has mismatched IDs: {} ({}, expected {})",
                                                data.title,
                                                data.published_file_id,
                                                file_name.display()
                                            );
                                            eprintln!(
                                                "For now, we trust the file name over the reported ID."
                                            );
                                            data.published_file_id = file_name.to_string_lossy().parse::<u32>()
                                                .expect("steam workshop items should be contained in a folder named by its ID");
                                        }
                                        let compat = self
                                            .library
                                            .compatibility
                                            .entry(data.published_file_id)
                                            .or_default();
                                        compat.extend_with(xcom_mod::scan_compatibility(&entry));
                                        self.downloaded.insert(data.published_file_id, entry);
                                        self.metadata.insert(data.published_file_id, data);
                                    }
                                    Err(err) => {
                                        eprintln!("Error parsing XComMod metadata file: {err:?}")
                                    }
                                }
                            } else {
                                eprintln!("Unable to find XComMod file in {}", entry.display())
                            }
                        }
                        Ok(_) => (),
                        Err(err) => {
                            eprintln!("Error scanning files in {}: {err:?}", dir.display());
                        }
                    }
                }
            }
            Err(err) => {
                self.modal_stack.push(AppModal::ErrorMessage(
                    "Error Scanning Downloads".to_string(),
                    format!("App was unable to scan the downloads directory. Error:\n{err:?}"),
                ));
            }
        }

        for (id, path) in self.downloaded.iter() {
            if let Some(item) = self.library.items.get_mut(id) {
                item.path = path.clone();
            } else if let Some(details) = self.file_cache.get_details(*id) {
                self.library.items.insert(
                    *id,
                    library::LibraryItem {
                        id: *id,
                        title: details.title.clone(),
                        path: path.clone(),
                        needs_update: false,
                        selected: false,
                    },
                );
            } else {
                eprintln!("Missing details for item {id}");
            }
        }

        self.library
            .update_missing_dependencies(self.file_cache.clone(), &self.metadata);
        for profile in self.save.profiles.values_mut() {
            profile.update_compatibility_issues(
                self.file_cache.clone(),
                &self.metadata,
                &self.library,
            );
        }
    }

    fn item_downloaded(&self, id: u32) -> bool {
        if self.is_downloading(id) {
            return false;
        }

        // Heuristically check if download was incomplete,
        // within 10% of expected size in case Steam is not reporting
        // sizes correctly
        if let Some(data) = self.file_cache.get_details(id)
            && let Ok(expected_size) = data.file_size.parse::<u64>()
        {
            if let Ok(size) = files::get_size(files::get_item_directory(
                &self.settings.download_directory,
                id,
            )) && size >= (expected_size as f32 * 0.9) as u64
            {
                return true;
            } else {
                return false;
            }
        }

        std::fs::exists(files::get_item_directory(
            &self.settings.download_directory,
            id,
        ))
        .ok()
        .is_some_and(|exists| exists)
    }

    fn cache_item_image(
        &self,
        info: &steam_rs::published_file_service::query_files::File,
    ) -> Task<Message> {
        if !self.images.contains_key(&info.preview_url) {
            let url = info.preview_url.clone();
            Task::future(async move {
                match web::load_image(&url).await {
                    Ok(path) => {
                        // Not sure if this is strictly necessary but for preventing overload
                        tokio::time::sleep(std::time::Duration::from_millis(16)).await;
                        Message::ImageLoaded(url, path)
                    }
                    Err(err) => {
                        eprintln!("Error attempting to load image: {err:?}");
                        Message::None
                    }
                }
            })
        } else {
            Task::none()
        }
    }

    fn get_item_directory(&mut self, id: u32) -> Option<PathBuf> {
        if let Some(path) = self.downloaded.get(&id) {
            return Some(path.to_owned());
        }

        let path = files::get_item_directory(&self.settings.download_directory, id);
        if let Ok(true) = std::fs::exists(&path) {
            self.downloaded.insert(id, path.clone());
            return Some(path);
        }

        None
    }

    fn setup_launch_log_monitor(&mut self) -> Task<Message> {
        if let Some(handle) = self.abortable_handles.remove(&AppAbortKey::GameLogMonitor) {
            handle.abort();
        }

        if let Some(local) = &self.save.local_directory {
            let path = local.join("Logs/Launch.log");
            let (task, handle) = files::monitor_file_changes(path.clone());
            self.abortable_handles
                .insert(AppAbortKey::GameLogMonitor, handle);
            task.map(|change| match change {
                files::MonitorFileChange::Create(s) => Message::LaunchLogCreated(s),
                files::MonitorFileChange::Append(s) => Message::LaunchLogAppended(s),
            })
        } else {
            Task::none()
        }
    }
}

#[cfg(target_os = "linux")]
struct ConnectionRecipe(zbus::blocking::Connection);
#[cfg(target_os = "linux")]
impl iced::advanced::subscription::Recipe for ConnectionRecipe {
    type Output = Message;
    fn hash(&self, state: &mut iced::advanced::subscription::Hasher) {
        // Allow only the singular instance
        struct ReceiverRecipeMarker;
        std::any::TypeId::of::<ReceiverRecipeMarker>().hash(state);
    }

    fn stream(
        self: Box<Self>,
        _input: iced::advanced::subscription::EventStream,
    ) -> iced::advanced::graphics::futures::BoxStream<Self::Output> {
        let connection = zbus::Connection::from(self.0);
        Box::pin(stream::channel(
            100,
            |mut output: Sender<Message>| async move {
                match connection
                    .object_server()
                    .at(
                        APP_SESSION_PATH,
                        AppSessionInterface {
                            sender: output.clone(),
                        },
                    )
                    .await
                {
                    Ok(true) => (), // Dbus setup succeeded,
                    Ok(false) => {
                        let _ = output
                            .send(Message::DbusError(zbus::Error::NameTaken))
                            .await;
                    }
                    Err(err) => {
                        let _ = output.send(Message::DbusError(err)).await;
                    }
                };
            },
        ))
    }
}

fn modal<'a>(
    showing: bool,
    base: impl Into<Element<'a, Message>>,
    // content: impl Into<Element<'a, Message>>,
    content: impl Iterator<Item = Element<'a, Message>>,
) -> Stack<'a, Message> {
    if showing {
        stack!(base.into()).extend(content.map(|item| {
            opaque(
                container(item)
                    .center_x(Fill)
                    .center_y(Fill)
                    .style(|_| container::Style {
                        background: Some(iced::Background::Color(
                            iced::Color::BLACK.scale_alpha(0.5),
                        )),
                        ..Default::default()
                    }),
            )
        }))
    } else {
        stack!(base.into())
    }
}

fn centered<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).center(Fill).into()
}

fn modal_box<'a>(
    height: impl Into<Length>,
    width: impl Into<Length>,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    container(content)
        .height(height)
        .width(width)
        .style(|theme: &Theme| container::Style {
            text_color: Some(theme.palette().primary.inverse()),
            background: Some(iced::Background::Color(theme.palette().primary)),
            border: iced::border::rounded(10),
            ..Default::default()
        })
        .into()
}

// At the moment, only for allowing one instance to be open at a time
// Doubles as a showcase of how to set up a dbus connection/service for iced
#[cfg(target_os = "linux")]
struct AppSessionInterface {
    sender: Sender<Message>,
}

#[cfg(target_os = "linux")]
#[zbus::interface(name = "io.github.phantomshift.lxcomm")]
impl AppSessionInterface {
    async fn focus_window(&mut self) {
        if let Err(err) = self.sender.send(Message::GainFocus).await {
            eprintln!("Error sending GainFocus message: {err:?}");
        }
    }
}

// #[tokio::main]
fn main() -> eyre::Result<()> {
    // Potential TODO - Find a more graceful way of solving this cross-platform
    #[cfg(not(target_os = "linux"))]
    let lock = {
        let lock = single_instance::SingleInstance::new("LXCOMM_SESSION_LOCK");
        if let Ok(lock) = lock
            && lock.is_single()
        {
            lock
        } else {
            rfd::MessageDialog::new()
                .set_buttons(rfd::MessageButtons::Ok)
                .set_level(rfd::MessageLevel::Error)
                .set_title("Already Open")
                .set_description("There is already an instance of LXCOMM open.")
                .show();
            return Ok(());
        }
    };

    let result = {
        let icon = iced::window::icon::from_file_data(
            include_bytes!("../assets/lxcomm_icon64.png"),
            Some(iced::advanced::graphics::image::image_rs::ImageFormat::Png),
        )?;

        #[cfg(target_os = "linux")]
        let connection = {
            let connection = zbus::blocking::Connection::session()?;
            if let Err(err) = connection.request_name(APP_SESSION_NAME) {
                match err {
                    zbus::Error::NameTaken => {
                        connection
                            .call_method(
                                Some(APP_SESSION_NAME),
                                APP_SESSION_PATH,
                                Some(APP_SESSION_NAME),
                                "FocusWindow",
                                &"",
                            )
                            .expect("failed to call focus method");
                        return Ok(());
                    }
                    err => return Err(eyre::Error::new(err)),
                }
            }
            connection
        };

        // Based on this post for passing initial state
        // https://discourse.iced.rs/t/solved-new-boot-trait-no-longer-able-to-use-a-capturing-closure-to-initialize-application-state/1012/6
        // Note that this does make the application incompatible
        // with iced's native debugging tool comet since it supposedly calls boot for re-initializing from the start.
        #[cfg(target_os = "linux")]
        let once_boot = RefCell::new(Some(App::boot(connection)?));
        #[cfg(not(target_os = "linux"))]
        let once_boot = RefCell::new(Some(App::boot()?));
        Ok((once_boot, icon))
    };

    let (once_boot, icon) = match result {
        Ok(ok) => ok,
        Err(err) => {
            rfd::MessageDialog::new()
                .set_buttons(rfd::MessageButtons::Ok)
                .set_level(rfd::MessageLevel::Error)
                .set_title("Startup Error")
                .set_description(indoc::formatdoc! {"
                    There was an unrecoverable error trying to start up LXCOMM; try launching again in a terminal for log output.
                    Error:
                    {err}
                "})
                .show();
            return Err(err);
        }
    };

    let boot = move || unsafe {
        // Safety - State was wrapped in the some value above
        once_boot.borrow_mut().take().unwrap_unchecked()
    };

    let window_settings = iced::window::Settings {
        icon: Some(icon),
        ..Default::default()
    };

    let application = iced::application(boot, App::update, App::view)
        .window(window_settings)
        .title("Linux XCOM2 Mod Manager")
        .theme(App::theme)
        .subscription(App::subscription)
        .exit_on_close_request(false)
        .font(iced_aw::temp_fonts::REQUIRED_FONT_BYTES);

    // TODO - Figure out why icon fonts don't render properly on Windows.
    #[cfg(not(target_os = "windows"))]
    let application = application.font(iced_fonts::FONTAWESOME_FONT_BYTES);

    application.run()?;
    Ok(())
}
