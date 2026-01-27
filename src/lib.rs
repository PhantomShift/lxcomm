#![feature(try_blocks)]
// For windows soft links
#![cfg_attr(target_os = "windows", feature(junction_point))]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    hash::Hash,
    ops::Not,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, LazyLock},
    time::Duration,
};

use apply::Apply;
use bevy_reflect::{GetField, NamedField, Reflect, StructInfo, Type, TypeInfo, Typed};
use bstr::{BString, ByteSlice};
use derivative::Derivative;
use etcetera::{AppStrategy, AppStrategyArgs};
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
    widget::{
        self, Stack, button, checkbox, column, combo_box, container, image, markdown, opaque,
        pane_grid, pick_list, progress_bar, rich_text, row, rule, scrollable, space, span, stack,
        table, text, text_editor, text_input, toggler, tooltip,
    },
};
use iced_aw::{card, widget::LabeledFrame};
use indexmap::IndexSet;
use itertools::Itertools;
use ringmap::RingMap;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use steam_rs::{self, Steam, published_file_service::query_files::PublishedFiles};
use strum::{Display, EnumIter, IntoEnumIterator};
use zip::ZipArchive;

use crate::{
    browser::WorkshopBrowser,
    collections::{Collection, CollectionsMessage, CollectionsState, ImageSource},
    extensions::Truncatable,
    files::ModDetails,
    library::LibraryItem,
    platform::symbols,
    snapshot::SnapshotContainer,
    widgets::{AsyncDialog, AsyncDialogField},
    xcom_mod::ModId,
};
use crate::{collections::CollectionSource, mod_edit::EditorMessage};
use crate::{extensions::DetailsExtension, web::resolve_all_dependencies};

#[cfg(target_os = "linux")]
use crate::platform::extensions::NotificationExtLinux;

pub mod browser;
pub mod collections;
pub mod extensions;
pub mod files;
pub mod library;
pub mod loading;
pub mod markup;
pub mod metadata;
pub mod mod_edit;
pub mod platform;
pub mod snapshot;
pub mod steam_manifest;
pub mod steamcmd;
pub mod web;
pub mod widgets;
pub mod xcom_mod;

const XCOM_APPID: u32 = 268500;

static APP_STRATEGY_ARGS: LazyLock<AppStrategyArgs> = LazyLock::new(|| AppStrategyArgs {
    author: "phantomshift".to_string(),
    top_level_domain: "io.github".to_string(),
    app_name: "lxcomm".to_string(),
});

#[cfg(target_os = "linux")]
static APP_SESSION_NAME: &str = "io.github.phantomshift.lxcomm";
#[cfg(target_os = "linux")]
static APP_SESSION_PATH: &str = "/io/github/phantomshift/lxcomm";

// Potential TODO - Enable dbus integration
#[cfg(feature = "flatpak")]
static FLATPAK_ID: LazyLock<Box<str>> = LazyLock::new(|| {
    std::env::var("FLAPTAK_ID")
        .unwrap_or(APP_SESSION_NAME.to_string())
        .into_boxed_str()
});

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

static LOCAL_COLLECTIONS_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let collections = DATA_DIR.join("collections");
    std::fs::create_dir_all(&collections).expect("data directory should be writable");
    collections
});

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
    AddModDirectory,
    AddModDirectoryConfirmed(PathBuf),
    RemoveModDirectory(usize),
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

    AsyncChooseResolve(AsyncDialogKey, usize),
    AsyncChooseCancelled(AsyncDialogKey),
    AsyncDialogUpdate(AsyncDialogKey, String, AsyncDialogField),
    AsyncDialogResolved(AsyncDialogKey, bool),
    AbortTask(AppAbortKey),

    ApiKeyRequest,
    ApiKeyRequestUpdate(String),
    ApiKeySubmit,

    SetBrowsePage(u32),
    SetViewingItem(ModId),
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
    DownloadAllRequested(u32),
    DownloadMultipleRequested(Vec<u32>),
    DownloadAllConfirmed(Arc<BTreeSet<u32>>),
    DownloadPushPending(Vec<u32>),

    Collections(CollectionsMessage),
    Settings(SettingsMessage),
    ProfileSettings(String, library::ProfileSettingsMessage),
    ModEditor(EditorMessage),

    LibraryToggleItem(ModId, bool),
    LibraryToggleAll(bool),
    LibraryUpdateRequest,
    LibraryScanRequest,
    LibraryCheckOutdatedRequest,
    LibraryForceCheckOutdated,
    CheckOutdatedCompleted(HashMap<u32, u64>),
    LibraryAddToProfileRequest,
    LibraryAddToProfileToggleAll(bool),
    LibraryAddToProfileToggled(String, bool),
    LibraryAddToProfileConfirm(Vec<ModId>),
    LibraryDeleteRequest,
    LibraryDeleteConfirm,
    LibraryFilterUpdateQuery(String),
    LibraryFilterToggleFuzzy(bool),
    ProfileAddPressed,
    ProfileAddEdited(String),
    ProfileAddCompleted(bool),
    ProfileAddItems(String, Vec<ModId>),
    ProfileDeletePressed(String),
    ProfileDeleteConfirmed(String),
    ProfileRemoveItems(String, Vec<ModId>),
    ProfileSelected(String),
    ProfileItemSelected(ModId),
    ProfileViewFolderRequested(String, String),
    ProfileImportFolderRequested(String, String),
    ProfileImportCollectionRequested(Arc<Collection>),
    ProfilePageResized(pane_grid::ResizeEvent),
    ProfileViewDetails(String),
    ProfileExportSnapshotRequested(String),
    ProfileImportSnapshotRequested,
    ProfileImportBasicSnapshot(String, Arc<ZipArchive<std::fs::File>>),
    ProfileImportIncrementalSnapshot(String, Arc<ZipArchive<std::fs::File>>),
    ProfileImportSnapshotCompleted(library::Profile),
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
    LoadToggleLinuxNative(bool),
    LoadLaunchGame,

    LoadFindGameRequested,
    LoadFindGameMatched(PathBuf),
    LoadFindGameResolved(PathBuf),
    LoadFindLocalRequested,
    LoadFindLocalMatched(PathBuf),
    LoadFindLocalResolved(PathBuf),

    ItemDetailsAddToLibraryRequest(Vec<ModId>),
    ItemDetailsAddToLibraryAllRequest(u32),

    SetPage(AppPage),
    SetBusy(bool),
    SetBusyMessage(String),
    ModifyBusyMessage(String),
    DisplayError(String, String),
    OpenModal(Arc<AppModal>),
    CloseModal,
    EscapeModal,

    // Subscription-related
    #[cfg(feature = "dbus")]
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

    fn busy_message<M: Into<String>>(message: M) -> Self {
        Self::SetBusyMessage(message.into())
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
    // Potential TODO: just collapse this into one page
    // with a toggle to switch between the two?
    #[strum(to_string = "Workshop Items")]
    Browse,
    #[strum(to_string = "Workshop Collections")]
    Collections,
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
    AddToProfileRequest(Vec<ModId>),
    LibraryDeleteRequest,
    SteamGuardCodeRequest,
    ProfileAddRequest,
    ProfileDetails(String),
    ItemDetailedView(ModId),
    CollectionDetailedView(CollectionSource),
    Busy,
    BusyMessage(String),
    ErrorMessage(String, String),
    AsyncChoose {
        title: String,
        body: String,
        #[derivative(PartialEq = "ignore")]
        sender: Sender<usize>,
        options: Vec<String>,
        key: AsyncDialogKey,
        strategy: AsyncDialogStrategy,
    },
    AsyncDialog(AsyncDialog),
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AsyncDialogKey {
    GameFind,
    LocalFind,
    DownloadAllConfirm,
    DownloadMultipleConfirm,
    ProfileImportCollection,
    ProfileImportSnapshot,
    Id(usize),
    StringId(String),
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
            ProfileDetails(_) => false,
            ItemDetailedView(_) => true,
            CollectionDetailedView(_) => true,
            Busy => false,
            BusyMessage(_) => false,
            ErrorMessage(_, _) => true,
            AsyncChoose { .. } => false,
            AsyncDialog(_) => false,
        }
    }

    fn async_choose<T: Into<String>, B: Into<String>>(
        key: AsyncDialogKey,
        title: T,
        body: B,
        options: Vec<String>,
        strategy: AsyncDialogStrategy,
    ) -> (Self, Receiver<usize>) {
        let (sender, receiver) = iced::futures::channel::mpsc::channel(1);
        (
            Self::AsyncChoose {
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

#[serde_with::serde_as]
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
struct AppSave {
    username: String,
    #[serde(rename = "profiles")]
    _profiles: Option<BTreeMap<usize, library::Profile>>,
    #[serde(default)]
    tracked_profiles: BTreeSet<String>,
    #[serde_as(as = "serde_with::DefaultOnError")]
    active_profile: Option<String>,
    game_directory: Option<PathBuf>,
    local_directory: Option<PathBuf>,
    launch_command: Option<String>,
    #[serde(default)]
    launch_args: Vec<(String, String)>,
    #[serde(default)]
    local_mod_dirs: IndexSet<PathBuf>,
    #[serde(default)]
    linux_native_mode: bool,
}

impl AppSave {
    /// Potential TODO: figure out config/save versioning and migration for removed/renamed fields?
    /// This is currently a one-off fix for profile migration from 0.3.*.
    /// The alternative is requiring user intervention, perhaps provide scripts.
    fn _migrate_profiles(&mut self) -> Option<HashMap<usize, library::Profile>> {
        let tracked = &mut self.tracked_profiles;
        self._profiles.as_ref().map(|profiles| {
            profiles
                .iter()
                .map(move |(id, profile)| {
                    tracked.insert(profile.name.clone());
                    (*id, profile.to_owned())
                })
                .collect()
        })
    }
}

pub struct App {
    #[cfg(feature = "dbus")]
    dbus_connection: zbus::blocking::Connection,

    api_key: SecretString,
    current_page: AppPage,
    modal_stack: Vec<AppModal>,
    browsing_scroll_id: iced::widget::Id,
    browsing_page: u32,
    browsing_page_max: u32,
    browsing_query: web::WorkshopQuery,
    browsing_query_period: web::WorkshopTrendPeriod,
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
    pending_queue: BTreeMap<u32, u64>,
    ongoing_download: Option<(u32, u64)>,
    completed_downloads: BTreeSet<u32>,
    errorred_downloads: BTreeMap<u32, String>,
    cancel_download_handles: HashMap<u32, iced::task::Handle>,
    downloaded: BTreeMap<u32, PathBuf>,
    local: BTreeSet<PathBuf>,
    library: library::Library,
    profiles: BTreeMap<String, library::Profile>,
    file_cache: files::Cache,
    markup_cache: markup::MarkupCache,
    profile_add_name: String,
    selected_profile_name: Option<Box<str>>,
    background_resolver_sender: Option<Sender<Message>>,
    mod_editor: mod_edit::Editor,
    active_profile_combo: combo_box::State<String>,
    abortable_handles: HashMap<AppAbortKey, iced::task::Handle>,
    launch_log: iced::widget::text_editor::Content,
    collections: CollectionsState,

    profile_pane_state: widgets::ProfilePaneState,
}

#[derive(Debug, Reflect, Clone)]
enum AppSettingEditor {
    StringInput,
    SecretInput,
    NumberInput(std::ops::RangeInclusive<u32>),
    BoolToggle,
    StringPick(Vec<&'static str>),
}

trait AppSettingsEditorTrait<T> {
    fn render<'a, M, F>(
        &'a self,
        field: &'a NamedField,
        name: &'static str,
        current: &'a T,
        can_edit: bool,
        on_update: F,
    ) -> Element<'a, Message>
    where
        M: Into<Message> + 'static,
        F: Fn(&'static str, T) -> M + 'a;
}

impl AppSettingsEditorTrait<String> for AppSettingEditor {
    fn render<'a, M, F>(
        &'a self,
        _field: &'a NamedField,
        name: &'static str,
        current: &'a String,
        can_edit: bool,
        on_update: F,
    ) -> Element<'a, Message>
    where
        M: Into<Message> + 'static,
        F: Fn(&'static str, String) -> M + 'a,
    {
        match self {
            AppSettingEditor::SecretInput | AppSettingEditor::StringInput => {
                let is_secret = matches!(self, AppSettingEditor::SecretInput);
                text_input("", current)
                    .secure(is_secret)
                    .on_input_maybe(can_edit.then_some(move |new| on_update(name, new).into()))
                    .into()
            }
            AppSettingEditor::StringPick(options) => {
                pick_list(options.as_slice(), Some(current.as_str()), move |new| {
                    on_update(name, new.to_owned()).into()
                })
                .width(Fill)
                .into()
            }
            _ => panic!(
                "String editor should only be used with StringInput, SecretInput or StringPick"
            ),
        }
    }
}

impl AppSettingsEditorTrait<bool> for AppSettingEditor {
    fn render<'a, M, F>(
        &'a self,
        _field: &'a NamedField,
        name: &'static str,
        current: &'a bool,
        can_edit: bool,
        on_update: F,
    ) -> Element<'a, Message>
    where
        M: Into<Message> + 'static,
        F: Fn(&'static str, bool) -> M + 'a,
    {
        match self {
            AppSettingEditor::BoolToggle => container(widget::toggler(*current).on_toggle_maybe(
                can_edit.then_some(move |toggled| on_update(name, toggled).into()),
            ))
            .center_y(Fill)
            .into(),
            _ => panic!("Bool edit should only be used with BoolToggle"),
        }
    }
}

impl AppSettingsEditorTrait<u32> for AppSettingEditor {
    fn render<'a, M, F>(
        &'a self,
        field: &'a NamedField,
        name: &'static str,
        current: &'a u32,
        can_edit: bool,
        on_update: F,
    ) -> Element<'a, Message>
    where
        M: Into<Message> + 'static,
        F: Fn(&'static str, u32) -> M + 'a,
    {
        match self {
            AppSettingEditor::NumberInput(range) => {
                let editor = iced_aw::number_input(current, range.to_owned(), |_| Message::None)
                    .on_input_maybe(can_edit.then_some(move |new| on_update(name, new).into()))
                    .width(200);

                if let Some(render_preview) = field.get_attribute::<AppSettingsNumberPreview>() {
                    row![
                        text!("{} ", render_preview.generate_preview(*current))
                            .height(Fill)
                            .center(),
                        editor
                    ]
                    .into()
                } else {
                    editor.into()
                }
            }
            _ => panic!("Number edit should only be used with NumberInput"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum AppSettingEdit {
    String(&'static str, String),
    Number(&'static str, u32),
    Bool(&'static str, bool),
}

impl AppSettingEditor {
    pub fn get_default_for(type_info: &'static TypeInfo) -> &'static AppSettingEditor {
        macro_rules! is_of {
            ($ty:ident,$type:ty) => {
                $ty == Type::of::<$type>()
            };
        }
        match *type_info.ty() {
            ty if is_of!(ty, String) => &AppSettingEditor::StringInput,
            ty if is_of!(ty, bool) => &AppSettingEditor::BoolToggle,
            ty if is_of!(ty, u32) => &AppSettingEditor::NumberInput(0..=u32::MAX),
            _ => {
                unimplemented!(
                    "default editor is not specified for type {}",
                    type_info.type_path()
                );
            }
        }
    }
}

pub fn app_field_label(field: &NamedField) -> Option<Element<'_, Message>> {
    let display = field.get_attribute::<AppSettingsLabel>()?;
    let description = field.get_attribute::<AppSettingsDescription>();
    Some(
        tooltip(
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
        .into(),
    )
}

#[derive(Debug, Reflect)]
struct AppSettingsLabel(&'static str);

#[derive(Debug, Reflect)]
struct AppSettingsDescription(&'static str);

#[derive(Debug, Reflect)]
struct AppSettingsDepends(Vec<&'static str>);

#[derive(Debug, Reflect)]
struct AppSettingsConflicts(Vec<&'static str>);

/// Indicates that a setting cannot be changed in a flatpak context.
#[derive(Debug, Reflect)]
struct AppSettingsFlatpakLocked;

#[derive(Debug, Reflect)]
enum AppSettingsNumberPreview {
    HumanTime,
    FileSize,
}

impl AppSettingsNumberPreview {
    fn generate_preview(&self, value: u32) -> String {
        match self {
            Self::HumanTime => {
                humantime::format_duration(Duration::from_secs(value as u64)).to_string()
            }
            Self::FileSize => files::SizeDisplay::automatic(value as u64).to_string(),
        }
    }
}

#[derive(Derivative, Reflect, PartialEq, Serialize, Deserialize, Clone)]
#[derivative(Debug)]
#[serde(default)]
struct AppSettings {
    #[reflect(@AppSettingsLabel("Download Directory"))]
    #[reflect(@AppSettingsDescription("Note: Already downloaded files are not automatically moved if you change this value."))]
    #[reflect(@AppSettingsFlatpakLocked)]
    download_directory: String,

    #[reflect(@AppSettingsLabel("Automatic Download Retries"))]
    #[reflect(@AppSettingsDescription(r#"Maximum amount of retries when attempting to download an item.
This is largely relevant for large mods where steamcmd will simply timeout the connection and stop the download,
although this will also retry if any other errors occur. Set this to a higher value if you are finding that mods fail to download consistently."#))]
    #[reflect(@AppSettingEditor::NumberInput(0..=20))]
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

    #[reflect(@AppSettingsLabel("Reload Profile on Launch"))]
    #[reflect(@AppSettingsDescription("If enabled, automatically sets up the selected active profile."))]
    reload_profile_on_launch: bool,

    #[reflect(@AppSettingsLabel("steamcmd: Command Path"))]
    #[reflect(@AppSettingsDescription(r#"If empty (default), attempts to find it in your path using 'which "steamcmd"'"#))]
    #[reflect(@AppSettingsFlatpakLocked)]
    steamcmd_command_path: String,
    #[reflect(@AppSettingsLabel("steamcmd: Login On Startup"))]
    steamcmd_login_on_startup: bool,
    #[reflect(@AppSettingsLabel("steamcmd: Logout On Exit"))]
    #[reflect(@AppSettingsDescription(r#"If enabled, runs 'logout' when closing if not currently busy with another operation,
uncaching your login details and requiring that you log in again manually next time you use steamcmd."#))]
    steamcmd_logout_on_exit: bool,
    #[reflect(@AppSettingsLabel("steamcmd: Save Password"))]
    #[reflect(@AppSettingsDescription("If enabled, saves your Steam password to your secrets wallet to be used when opening the app again."))]
    steamcmd_save_password: bool,

    #[reflect(@AppSettingsLabel("Steam Web API: Save API Key"))]
    #[reflect(@AppSettingsDescription("If enabled, saves your Steam web API key to your secrets wallet to be used when opening the app again."))]
    steam_webapi_save_api_key: bool,
    #[reflect(@AppSettingsLabel("Steam Web API: Query Cache Lifetime (seconds)"))]
    #[reflect(@AppSettingEditor::NumberInput(0..=604800))]
    #[reflect(@AppSettingsNumberPreview::HumanTime)]
    steam_webapi_cache_lifetime: u32,
    #[reflect(@AppSettingsLabel("Steam Web API: API Key"))]
    #[reflect(@AppSettingEditor::SecretInput)]
    #[serde(skip)]
    #[derivative(Debug = "ignore")]
    steam_webapi_api_key: String,
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
            automatic_download_retries: 1,
            check_updates_on_startup: true,
            notify_on_download_complete: true,
            notify_with_sound: false,
            notify_progress: false,
            reload_profile_on_launch: true,

            steamcmd_login_on_startup: false,
            steamcmd_command_path: Default::default(),
            steamcmd_logout_on_exit: false,
            steamcmd_save_password: false,

            steam_webapi_save_api_key: false,
            steam_webapi_cache_lifetime: 86400,
            steam_webapi_api_key: Default::default(),
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
        #[cfg(feature = "dbus")] connection: zbus::blocking::Connection,
    ) -> eyre::Result<(Self, Task<Message>)> {
        let mut reportable_errors = Vec::new();

        let steam_web_api_entry = keyring::Entry::new("lxcomm-steam", "steam-web-api")?;
        let steam_password_entry = keyring::Entry::new("lxcomm-steam", "steam-password")?;

        fn load_data<T: Default + DeserializeOwned>(
            source: &Path,
            name: &'static str,
        ) -> eyre::Result<T> {
            let res: std::io::Result<T> = try {
                if !source.try_exists()? {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "could not find data",
                    ))?
                } else {
                    let file = std::fs::File::open(source)?;
                    let data: T = serde_json::from_reader(file).map_err(std::io::Error::from)?;
                    data
                }
            };

            match res {
                Ok(data) => Ok(data),
                Err(err) => {
                    let backup = source.with_added_extension("bak");
                    if err.kind() != std::io::ErrorKind::NotFound {
                        if std::fs::copy(source, &backup).is_err() {
                            return Err(eyre::Report::new(err)
                                    .wrap_err(format!(
                                        "Failed to load {name} and could not automatically create a backup at {}, {name} will be lost if you do not back it up manually before closing LXCOMM.",
                                        backup.display()
                                    )
                                )
                            );
                        } else {
                            return Err(eyre::Report::new(err).wrap_err(format!(
                                "Failed to load {name}, original has been backed up to {}.",
                                backup.display()
                            )));
                        }
                    };

                    Ok(T::default())
                }
            }
        }

        let mut settings = load_data::<AppSettings>(SETTINGS_PATH.as_path(), "settings")
            .unwrap_or_else(|report| {
                reportable_errors.push(report);
                AppSettings::default()
            });

        let mut save = load_data::<AppSave>(SAVE_PATH.as_path(), "application data")
            .unwrap_or_else(|report| {
                reportable_errors.push(report);
                AppSave::default()
            });

        if let Some(old_profiles) = save._migrate_profiles() {
            eprintln!("Migrating old profile data...");
            // Keep a backup just in case
            if let Err(err) = std::fs::File::create_new(SAVE_PATH.with_added_extension("bak"))
                .map_err(eyre::Error::new)
                .map(|file| serde_json::to_writer_pretty(file, &save).map_err(eyre::Error::new))
            {
                eprintln!(
                    "Error creating backup of save data being migrated from old version: {err:?}"
                );
            }

            for (id, mut profile) in old_profiles.into_iter() {
                let name = {
                    // In case user named their profile just a number
                    let mut original = library::sanitize_profile_name(&profile.name);
                    if original.parse::<usize>().is_ok() {
                        original = format!("Profile_{original}");
                    }
                    original
                };

                profile.name = name.clone();
                let old_path = PROFILES_DIR.join(id.to_string());
                let new_dest = PROFILES_DIR.join(&name);
                let data_path = new_dest.join(library::PROFILE_DATA_NAME);

                let res: std::io::Result<()> = {
                    if old_path.try_exists()? && !new_dest.try_exists()? {
                        std::fs::rename(&old_path, new_dest)?;
                        let writer = std::fs::File::create_new(data_path)?;
                        serde_json::to_writer_pretty(writer, &profile)?;
                    }
                    Ok(())
                };
                if let Err(err) = res {
                    reportable_errors.push(eyre::Error::new(err).wrap_err(format!(
                        "Error migrating profile '{name}', user will need to migrate manually."
                    )));
                }
            }

            save._profiles = None;
        };

        let profiles = BTreeMap::from_iter(save.tracked_profiles.iter().filter_map(|tracked| {
            match library::Profile::load_state(&PROFILES_DIR, tracked) {
                Ok(mut profile) => {
                    profile.settings_editing = profile.settings.clone();
                    Some((profile.name.clone(), profile))
                }
                Err(err) => {
                    reportable_errors.push(
                        eyre::Error::new(err)
                            .wrap_err(format!("Error occurred loading profile '{tracked}'")),
                    );
                    None
                }
            }
        }));

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
        let result: std::io::Result<()> = try {
            // TODO: Figure out why automatic conversion of errors in try blocks suddenly isnt working
            std::fs::create_dir_all(&*CONFIG_DIR)?;
            let file = std::fs::File::create(&*SETTINGS_PATH)?;
            serde_json::to_writer_pretty(file, &settings).map_err(std::io::Error::from)?;
        };
        if let Err(err) = result {
            eprintln!("Failed to overwrite settings on boot: {err}");
        };

        let active_profile_combo =
            combo_box::State::new(save.tracked_profiles.iter().cloned().collect());

        #[cfg(feature = "flatpak")]
        {
            settings.download_directory = AppSettings::default().download_directory;
            settings.steamcmd_command_path = String::from("/app/bin/steamcmd");
        }

        let mut app = App {
            #[cfg(feature = "dbus")]
            dbus_connection: connection,

            api_key: SecretString::default(),
            current_page: Default::default(),
            modal_stack: Vec::new(),
            browsing_scroll_id: iced::widget::Id::unique(),
            browsing_page: 0,
            browsing_page_max: 0,
            browsing_query: web::WorkshopQuery::default(),
            browsing_query_period: web::WorkshopTrendPeriod::default(),
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
            pending_queue: BTreeMap::new(),
            ongoing_download: None,
            completed_downloads: BTreeSet::new(),
            errorred_downloads: BTreeMap::new(),
            cancel_download_handles: HashMap::new(),
            downloaded: BTreeMap::new(),
            local: BTreeSet::new(),
            library: library::Library::default(),
            profiles,
            file_cache: files::Cache::default(),
            markup_cache: markup::MarkupCache::default(),
            profile_add_name: String::new(),
            selected_profile_name: None,
            background_resolver_sender: None,
            mod_editor: mod_edit::Editor::default(),
            active_profile_combo,
            abortable_handles: HashMap::new(),
            launch_log: Default::default(),
            collections: CollectionsState::default(),

            profile_pane_state: Default::default(),
        };

        let auto_grab_api_key = match app.credentials.steam_web_api.get_password() {
            _ if !app.settings.steam_webapi_save_api_key => Task::done(Message::ApiKeyRequest),

            Ok(key) if key.is_empty() => Task::done(Message::ApiKeyRequest),
            Ok(key) => {
                app.settings.steam_webapi_api_key = key.clone();
                app.settings_editing.steam_webapi_api_key = key.clone();
                app.api_key = SecretString::from(key);
                if app.settings.check_updates_on_startup {
                    Task::done(Message::LibraryForceCheckOutdated)
                } else {
                    Task::none()
                }
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

        let auto_login = if app.settings.steamcmd_login_on_startup && !app.save.username.is_empty()
        {
            Task::done(Message::SteamCMDLogin(false))
        } else {
            Task::none()
        };

        let display_errors =
            if reportable_errors.is_empty() {
                Task::none()
            } else {
                Task::batch(reportable_errors.into_iter().map(|err| {
                    Task::done(Message::display_error("Startup Error", format!("{err:?}")))
                }))
            };

        // Additional startup tasks
        app.trim_snapshots();
        app.trim_image_cache();

        // Potential TODO: turn this into an asynchronous task to prevent UI lockup on extremely large libraries
        app.scan_downloads();
        for path in app.save.local_mod_dirs.clone() {
            app.scan_mods(&path, false);
        }

        Ok((
            app,
            Task::batch([auto_grab_api_key, auto_login, display_errors]),
        ))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            // TODO - Figure out a good way to decouple this
            Message::Collections(message) => return self.update_collections(message),
            Message::Settings(message) => return self.update_settings(message),
            Message::ProfileSettings(name, message) => {
                if let Some(profile) = self.profiles.get_mut(&name) {
                    return profile.handle_settings_update(message);
                }
            }
            Message::ModEditor(message) => {
                if let Some(task) = self.mod_editor.update(message) {
                    return task;
                }
            }
            Message::AsyncChooseResolve(key, choice) => {
                if let Some(index) =
                    self.modal_stack
                        .iter()
                        .enumerate()
                        .find_map(|(index, modal)| {
                            matches!(modal, AppModal::AsyncChoose { key: this_key, .. } if *this_key == key).then_some(index)
                        })
                    && let AppModal::AsyncChoose { mut sender, .. } = self.modal_stack.remove(index)
                {
                    return Task::future(async move {
                        if let Err(err) = sender.send(choice).await {
                            eprintln!("Error resolving async dialog: {err:?}")
                        }
                        Message::None
                    });
                }
            }
            Message::AsyncChooseCancelled(key) => {
                self.modal_stack.retain(|modal| !matches!(modal, AppModal::AsyncChoose { key: this, .. } if *this == key));
            }
            Message::AsyncDialogUpdate(key, field, value) => {
                if let Some(dialog) = self.modal_stack.iter_mut().find_map(|modal| match modal {
                    AppModal::AsyncDialog(dialog) if dialog.key == key => Some(dialog),
                    _ => None,
                }) {
                    dialog
                        .fields
                        .0
                        .entry(field)
                        .and_modify(|field| *field = value);
                }
            }
            Message::AsyncDialogResolved(key, submitted) => {
                if let Some(index) =
                    self.modal_stack
                        .iter()
                        .enumerate()
                        .find_map(|(index, modal)| {
                            matches!(modal, AppModal::AsyncDialog(dialog) if dialog.key == key)
                                .then_some(index)
                        })
                    && let AppModal::AsyncDialog(AsyncDialog {
                        fields, mut sender, ..
                    }) = self.modal_stack.remove(index)
                {
                    return Task::future(async move {
                        if let Err(err) = sender.send(submitted.then_some(fields)).await {
                            eprintln!("Error resolving async dialog: {err:?}")
                        }
                        Message::None
                    });
                }
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

                        tasks.push(self.cache_item_image(&details.preview_url));
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

                    tasks.push(self.cache_item_image(&details.preview_url));
                }
                return Task::batch(tasks);
            }

            Message::ApiKeyRequest => self.modal_stack.push(AppModal::ApiKeyRequest),
            Message::ApiKeyRequestUpdate(key) => {
                self.settings.steam_webapi_api_key = key.clone();
                self.settings_editing.steam_webapi_api_key = key.clone();
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

            Message::SetBrowsePage(new) => {
                let new = std::cmp::max(new, 1);
                if new == self.browsing_page {
                    return Task::none();
                }

                self.browsing_page = new;
                return Task::done(Message::BrowseUpdateQuery);
            }
            Message::SetViewingItem(id) => {
                self.modal_stack
                    .push(AppModal::ItemDetailedView(id.clone()));
                if let Some(ModDetails::Workshop(info)) = self.file_cache.get_details(&id) {
                    self.markup_cache.cache_markup(info.get_description());

                    let unknown = info
                        .children
                        .iter()
                        .filter_map(|c| {
                            let id = c
                                .published_file_id
                                .parse::<u32>()
                                .expect("ids should be numbers");
                            self.file_cache
                                .get_details(ModId::Workshop(id))
                                .is_none()
                                .then_some(id)
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
                            self.cache_item_image(&info.preview_url),
                        ]);
                    } else {
                        return self.cache_item_image(&info.preview_url);
                    }
                } else if let ModId::Workshop(id) = id
                    && let Some(mut sender) = self.background_resolver_sender.clone()
                {
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
                } else if let Some(ModDetails::Local(data)) = self.file_cache.get_details(&id) {
                    self.markup_cache.cache_markup(data.description);
                }
            }
            Message::BrowseEditQuery(query) => self.browsing_query.query = query,
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
                self.browsing_page = 1;

                return Task::done(Message::BrowseUpdateQuery);
            }
            Message::BrowseUpdateQuery => {
                let page = self.browsing_page;
                let query = self.browsing_query.clone();
                let cache_lifetime = self.settings.steam_webapi_cache_lifetime;
                let api_key = self.api_key.clone();
                let scroll_task = reset_scroll!(self.browsing_scroll_id.clone());
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
                    .chain(scroll_task)
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
            Message::ModifyBusyMessage(message) => {
                if let Some(AppModal::BusyMessage(current)) = self.modal_stack.last_mut() {
                    *current = message;
                }
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

                let Some(session) = &self.steamcmd_state.session else {
                    return Task::done(Message::display_error(
                        "Unexpected State",
                        "Something has gone wrong initializing SteamCMD. Please report this to the dev.",
                    ));
                };
                let init_logs = session.consume_buffer();
                let mut event_receiver = session.event_receiver.resubscribe();
                let logging_task = Task::stream(iced::stream::channel(
                    16,
                    async move |mut output| {
                        for line in init_logs {
                            let _ = output.send(Message::LogAppend(line.into())).await;
                        }

                        while let Ok(event) = event_receiver.recv().await {
                            match event {
                                steamcmd::SessionEvent::Line(line) => {
                                    if let Err(err) =
                                        output.send(Message::LogAppend(line.into())).await
                                    {
                                        eprintln!("Error sending output: {err:?}");
                                    }
                                }
                                // TODO - Add sequence for recovering from unexpected shutdown
                                steamcmd::SessionEvent::Shutdown => {
                                    let _ = output.send(Message::display_error(
                                        "SteamCMD Session Closed Unexpectedly",
                                        "The SteamCMD session has stopped for some reason.\nYou will not be able to download mods until you restart.",
                                    )).await;
                                }
                                _ => (),
                            }
                        }
                    },
                ));

                if let Some(state) = Arc::get_mut(&mut self.steamcmd_state) {
                    state.is_cached = true;
                } else {
                    eprintln!("Failed to get exclusive ownership of steamcmd state...");
                }

                return Task::batch([self.queue_downloads(), logging_task]);
            }
            Message::SteamCMDLogin(manual) => {
                if self.steamcmd_state.is_logged_in() {
                    return Task::none();
                }

                if self.save.username.is_empty() {
                    return Task::none();
                }

                if self.steamcmd_state.session.is_none() {
                    // If there is no session, there should be no ongoing operations
                    // in other threads currently referencing steamcmd state.
                    let Some(state) = Arc::get_mut(&mut self.steamcmd_state) else {
                        return Task::done(Message::display_error(
                            "Error Logging In",
                            "SteamCMD session is in an unexpected state, report this to the dev.",
                        ));
                    };

                    let session = match steamcmd::Session::init(state.command_path.clone()) {
                        Ok(s) => s,
                        Err(err) => {
                            return Task::done(Message::display_error(
                                "Error Logging In",
                                format!("Failed to start SteamCMD session: {err:?}"),
                            ));
                        }
                    };
                    state.session.replace(session);
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
                    .push(AppModal::BusyMessage("Logging into SteamCMD...".into()));
                let state = self.steamcmd_state.clone();
                return Task::perform(
                    async move {
                        let handle = if let Some(mut sender) = state.message_sender.clone()
                            && let Some(mut lines) =
                                state.session.as_ref().map(|session| session.lines())
                        {
                            Some(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
                                async move {
                                    while let Some(line) = lines.next().await {
                                        static MATCH_STEAMCMD_UPDATE_PROG: LazyLock<
                                            fancy_regex::Regex,
                                        > = LazyLock::new(|| {
                                            fancy_regex::Regex::new("\\[....\\]")
                                                .expect("regex should be valid")
                                        });
                                        if MATCH_STEAMCMD_UPDATE_PROG
                                            .find(&line)
                                            .ok()
                                            .flatten()
                                            .is_some()
                                        {
                                            let _ = sender
                                                .send(Message::ModifyBusyMessage(format!(
                                                    "SteamCMD is updating...\n{line}"
                                                )))
                                                .await;
                                        }
                                    }
                                },
                            )))
                        } else {
                            None
                        };
                        let result = state.attempt_cached_login().await;
                        drop(handle);
                        result
                    },
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
                self.library.items.remove(&ModId::Workshop(id));
            }
            Message::DownloadCancelRequested(id) => {
                if let Some(_progress) = self.download_queue.remove(&id) {
                    self.errorred_downloads.insert(id, "Cancelled".to_string());
                }
            }
            Message::SteamCMDDownloadRequested(id) => {
                if !self.steamcmd_state.is_logged_in() {
                    return Task::done(Message::DisplayError(
                        "Error: Not Logged In".to_string(),
                        "You must be logged in to steamcmd to download files.".to_string(),
                    ));
                }

                if self.is_downloading(id) {
                    return Task::none();
                }

                self.errorred_downloads.remove(&id);

                if self.ongoing_download.is_none() {
                    return self.perform_download(id);
                }

                self.download_queue.insert(id, 0);
            }
            Message::SteamCMDDownloadCompleted(id) => {
                self.ongoing_download = None;
                self.completed_downloads.insert(id);
                self.scan_downloads();

                #[cfg(target_os = "linux")]
                if self.settings.notify_progress
                    && let Some(notif_id) = NOTIF_CACHE.remove(&id)
                    && let Some(details) = self.file_cache.get_details(ModId::Workshop(id))
                {
                    let mut notif = notify_rust::Notification::new();
                    let title = details.title();
                    notif
                        .appname("LXCOMM")
                        .icon("download")
                        .auto_desktop_entry()
                        .summary(&format!("Downloaded {title}"))
                        .body("Downloaded Completed")
                        .timeout(-1)
                        .id(notif_id);
                    let _ = notif.show();
                }

                if self.download_queue.is_empty() {
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
                                .hint(notify_rust::Hint::Resident(true))
                                .icon("download")
                                .auto_desktop_entry();

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
                    && let Some(details) = self.file_cache.get_details(ModId::Workshop(id))
                {
                    let mut notif = notify_rust::Notification::new();
                    let title = details.title();
                    notif
                        .appname("LXCOMM")
                        .icon("download")
                        .auto_desktop_entry()
                        .summary(&format!("Error Downloading {title}"))
                        .body(&message)
                        .timeout(-1)
                        .id(notif_id);
                    let _ = notif.show();
                }

                self.ongoing_download = None;
                self.errorred_downloads.insert(id, message);

                return self.queue_downloads();
            }
            Message::SteamCMDDownloadProgress(id, size) => {
                let Some((current, progress)) = self.ongoing_download.as_mut() else {
                    return Task::none();
                };
                if *current != id {
                    eprintln!("Mismatched IDs for download...? {id} vs {current}");
                    return Task::none();
                }
                *progress = size;

                #[cfg(target_os = "linux")]
                if self.settings.notify_progress
                    && let Some(ModDetails::Workshop(info)) =
                        self.file_cache.get_details(ModId::Workshop(id))
                    && let Ok(total_size) = info.file_size.parse::<u64>()
                {
                    let title = info.title.clone();
                    let progress = std::cmp::min(size * 100 / total_size, 0);
                    return Task::future(async move {
                        let mut notif = notify_rust::Notification::new();
                        notif
                            .appname("LXCOMM")
                            .icon("download")
                            .auto_desktop_entry()
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
                                format!("{} ({id})", cache.get_details(ModId::Workshop(id)).as_ref().map(|f| f.title()).unwrap_or("UNKNOWN"))
                            }).join("\n");
                            let (modal, mut rec) = AppModal::async_choose(
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
            Message::DownloadMultipleRequested(ids) => {
                let ids = ids
                    .iter()
                    .filter(|id| !self.item_downloaded(**id))
                    .copied()
                    .collect::<BTreeSet<_>>();
                let (modal, mut rec) = AppModal::async_choose(
                    AsyncDialogKey::DownloadMultipleConfirm,
                    "Download All?",
                    ids.iter()
                        .map(|id| {
                            if let Some(details) = self.file_cache.get_details(ModId::from(id)) {
                                format!("{} ({id})", details.title())
                            } else {
                                format!("UNKNOWN ({id})")
                            }
                        })
                        .join(", "),
                    vec!["No".to_string(), "Yes".to_string()],
                    AsyncDialogStrategy::Replace,
                );

                return Task::done(Message::OpenModal(Arc::new(modal))).chain(Task::future(
                    async move {
                        match rec.next().await {
                            Some(1) => Message::DownloadAllConfirmed(Arc::new(ids)),
                            _ => Message::None,
                        }
                    },
                ));
            }
            Message::DownloadAllConfirmed(ids) => {
                return Task::batch(
                    ids.iter()
                        .map(|id| Task::done(Message::SteamCMDDownloadRequested(*id))),
                );
            }
            Message::DownloadPushPending(ids) => {
                for id in ids {
                    if let Some((id, _)) = self.pending_queue.remove_entry(&id) {
                        self.download_queue.push_back(id, 0);
                    }
                }
                return self.queue_downloads();
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
                if !self.steamcmd_state.is_logged_in() {
                    return Task::done(Message::DisplayError(
                        "Error".to_string(),
                        "You must log in to steamcmd in order to update!".to_string(),
                    ));
                }

                for id in self.library.iter_workshop_id_selected() {
                    self.download_queue.push_back(id, 0);
                }
                return self.queue_downloads();
            }
            Message::LibraryScanRequest => {
                self.scan_downloads();
            }
            Message::LibraryCheckOutdatedRequest => {
                let items = self
                    .library
                    .iter_workshop_id_selected()
                    .filter(|&id| !self.is_downloading(id) && !self.pending_queue.contains_key(&id))
                    .map(|id| {
                        (
                            id as u64,
                            metadata::read_metadata(id, &self.settings.download_directory)
                                .map(|meta| meta.time_downloaded)
                                .unwrap_or_default(),
                        )
                    })
                    .collect::<Vec<_>>();
                let client = Steam::new(self.api_key.expose_secret());

                return Task::done(Message::busy_message("Checking for Outdated Mods..."))
                    .chain(Task::future(async move {
                        match web::check_mods_outdated(client, items).await {
                            Err(err) => Message::display_error(
                                "Error Checking Updates",
                                format!("{err:#?}"),
                            ),
                            Ok(outdated) => Message::CheckOutdatedCompleted(outdated),
                        }
                    }))
                    .chain(Task::done(Message::SetBusy(false)));
            }
            Message::LibraryForceCheckOutdated => {
                let items = self
                    .library
                    .items
                    .keys()
                    .filter_map(ModId::maybe_workshop)
                    .map(|id| {
                        (
                            id as u64,
                            metadata::read_metadata(id, &self.settings.download_directory)
                                .map(|meta| meta.time_downloaded)
                                .unwrap_or_default(),
                        )
                    })
                    .collect::<Vec<_>>();
                let client = Steam::new(self.api_key.expose_secret());

                return Task::done(Message::busy_message("Checking for Outdated Mods..."))
                    .chain(Task::future(async move {
                        match web::check_mods_outdated(client, items).await {
                            Err(err) => Message::display_error(
                                "Error Checking Updates",
                                format!("{err:#?}"),
                            ),
                            Ok(outdated) => Message::CheckOutdatedCompleted(outdated),
                        }
                    }))
                    .chain(Task::done(Message::SetBusy(false)));
            }
            Message::CheckOutdatedCompleted(outdated) => {
                return if !outdated.is_empty() {
                    for (id, time) in outdated {
                        if self
                            .ongoing_download
                            .is_none_or(|(ongoing, _)| ongoing != id)
                            && !self.download_queue.contains_key(&id)
                        {
                            self.pending_queue.insert(id, time);
                        }
                    }

                    Task::done(Message::SetPage(AppPage::Downloads))
                } else {
                    Task::done(Message::display_error(
                        "No Updates",
                        "All mods that aren't downloading are up to date!",
                    ))
                };
            }
            Message::LibraryAddToProfileRequest => {
                for profile in self.profiles.values_mut() {
                    profile.add_selected = false;
                }
                self.modal_stack.push(AppModal::AddToProfileRequest(
                    self.library
                        .iter_selected()
                        .map(|item| item.id.clone())
                        .collect(),
                ));
            }
            Message::LibraryAddToProfileToggleAll(toggled) => {
                self.profiles
                    .values_mut()
                    .for_each(|p| p.add_selected = toggled);
            }
            Message::LibraryAddToProfileToggled(name, toggle) => {
                self.profiles
                    .entry(name)
                    .and_modify(|profile| profile.add_selected = toggle);
            }
            Message::LibraryAddToProfileConfirm(ids) => {
                let mut errors = Vec::new();
                for profile in self
                    .profiles
                    .values_mut()
                    .filter(|profile| profile.add_selected)
                {
                    for item in ids.iter() {
                        profile
                            .items
                            .entry(item.clone())
                            .or_insert_with(library::LibraryItemSettings::default);
                    }
                    profile.update_compatibility_issues(self.file_cache.clone(), &self.library);

                    if let Err(err) = profile.save_state_in(PROFILES_DIR.as_path()) {
                        errors.push(eyre::Error::new(err).wrap_err(format!("Failed to write to profile '{}', added mods will not be present when LXCOMM next launches.", profile.name)))
                    }
                }
                self.modal_stack.pop();
                for error in errors {
                    let _ = self.update(Message::display_error(
                        "Error Updating Profile",
                        format!("{error:?}"),
                    ));
                }
            }
            Message::LibraryDeleteRequest => self.modal_stack.push(AppModal::LibraryDeleteRequest),
            Message::LibraryDeleteConfirm => {
                self.modal_stack.pop();
                return Task::batch(
                    self.library
                        .iter_workshop_id_selected()
                        .map(|id| Task::done(Message::DeleteRequested(id))),
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
                    let name = &self.profile_add_name;

                    if name.is_empty() {
                        return Task::none();
                    }

                    if self.profiles.contains_key(name) {
                        return Task::done(Message::DisplayError(
                            "Error".to_string(),
                            "Profile with this name already exists!".to_string(),
                        ));
                    }

                    if let Err(message) = library::validate_profile_name(name) {
                        return Task::done(Message::display_error("Invalid Name", message));
                    }

                    let profile_path = PROFILES_DIR.join(name);
                    let profile = library::Profile {
                        name: name.to_owned(),
                        items: BTreeMap::new(),
                        ..Default::default()
                    };
                    let result: std::io::Result<()> = try {
                        if let Ok(true) = profile_path.try_exists() {
                            std::fs::remove_dir_all(&profile_path)?
                        }
                        std::fs::create_dir(&profile_path)?;
                        for name in library::profile_folder::ALL.iter() {
                            std::fs::create_dir(profile_path.join(name))?;
                        }
                        let writer = std::fs::File::create_new(
                            profile_path.join(library::PROFILE_DATA_NAME),
                        )?;
                        serde_json::to_writer_pretty(writer, &profile)
                            .map_err(std::io::Error::from)?;
                    };
                    if let Err(err) = result {
                        return Task::done(Message::display_error(
                            "Error Setting Up Profile",
                            format!("Error attempting to set up new profile: {err:?}"),
                        ));
                    }

                    self.profiles.insert(self.profile_add_name.clone(), profile);
                    self.save.tracked_profiles.insert(name.to_owned());
                    self.selected_profile_name = Some(name.to_owned().into_boxed_str());
                    self.active_profile_combo =
                        combo_box::State::new(self.save.tracked_profiles.iter().cloned().collect());
                }
                self.modal_stack.pop();
            }
            Message::ProfileAddEdited(name) => self.profile_add_name = name,
            Message::ProfileAddItems(name, items) => {
                let mut err = None;
                self.profiles.entry(name).and_modify(|profile| {
                    profile.items.extend(
                        items
                            .into_iter()
                            .map(|id| (id, library::LibraryItemSettings::default())),
                    );

                    profile.update_compatibility_issues(self.file_cache.clone(), &self.library);

                    err = profile.save_state_in(PROFILES_DIR.as_path()).err();
                });

                if let Some(err) = err {
                    return Task::done(Message::display_error(
                        "Error Updating Profile",
                        format!("Failed to write changes to disk: {err:?}"),
                    ));
                }
            }
            Message::ProfileDeletePressed(name) => {
                let (modal, mut rec) = AppModal::async_choose(
                    AsyncDialogKey::StringId(name.clone()),
                    "Delete Profile",
                    "Are you sure you want to delete this profile?",
                    vec!["No".to_string(), "Yes".to_string()],
                    AsyncDialogStrategy::Replace,
                );

                self.modal_stack.push(modal);
                return Task::future(async move {
                    if let Some(1) = rec.next().await {
                        Message::ProfileDeleteConfirmed(name)
                    } else {
                        Message::CloseModal
                    }
                });
            }
            Message::ProfileDeleteConfirmed(name) => {
                if self.profiles.remove(&name).is_some() {
                    self.save.tracked_profiles.remove(&name);
                    if Some(&name) == self.save.active_profile.as_ref() {
                        self.save.active_profile = None;
                    }
                }
            }
            Message::ProfileRemoveItems(name, items) => {
                let Some(profile) = self.profiles.get_mut(&name) else {
                    eprintln!("Attempted to access non-existent profile...");
                    return Task::none();
                };

                let path = PROFILES_DIR.join(&name).join(library::PROFILE_DATA_NAME);
                let Some(file) = path
                    .exists()
                    .then_some(std::fs::File::create(&path).ok())
                    .flatten()
                else {
                    return Task::done(Message::display_error(
                        "Write Error",
                        format!("Failed to open {}", path.display()),
                    ));
                };

                profile.items.retain(|id, _| !items.contains(id));
                profile.update_compatibility_issues(self.file_cache.clone(), &self.library);

                if let Err(err) = profile.save_into(file) {
                    return Task::done(Message::display_error(
                        "Profile Corrupted",
                        format!(
                            "Failed to write to {} when saving profile: {err:?}",
                            path.display()
                        ),
                    ));
                }

                if let Some(selected) = &self.selected_profile_name
                    && *name == **selected
                    && let Some(profile) = self.profiles.get_mut(selected.as_ref())
                    && profile
                        .view_selected_item
                        .as_ref()
                        .map(|id| items.iter().find(|i| *i == id))
                        .is_some()
                {
                    profile.view_selected_item = None;
                }
            }
            Message::ProfileSelected(name) => {
                self.selected_profile_name = Some(name.clone().into_boxed_str());
                if let Some(profile) = self.profiles.get(&name) {
                    let mut tasks = Vec::new();
                    for id in profile.items.keys() {
                        if let Some(ModDetails::Workshop(details)) = self.file_cache.get_details(id)
                        {
                            self.markup_cache.cache_markup(details.get_description());
                            tasks.push(self.cache_item_image(&details.preview_url));
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

                if let Some(selected) = &self.selected_profile_name
                    && let Some(profile) = self.profiles.get_mut(selected.as_ref())
                {
                    if Some(&id) == profile.view_selected_item.as_ref() {
                        profile.view_selected_item = None;
                    } else {
                        profile.view_selected_item = Some(id.clone());

                        if let Some(task) = self.mod_editor.update(mod_edit::EditorMessage::Load(
                            selected.to_string(),
                            id,
                            PathBuf::from(&self.settings.download_directory),
                        )) {
                            return task;
                        }
                    }
                }
            }
            Message::ProfileViewFolderRequested(profile_name, name) => {
                if !library::profile_folder::ALL.contains(&name.as_str()) {
                    return Task::done(Message::display_error(
                        "Does Not Exist",
                        "There is no such folder in a profile",
                    ));
                }

                let path = PROFILES_DIR.join(&profile_name).join(name);

                if let Err(err) = opener::open(&path) {
                    return Task::done(Message::display_error(
                        "Error Opening",
                        format!("Something went wrong opening {}:\n{err:?}", path.display()),
                    ));
                }
            }
            Message::ProfileImportFolderRequested(profile_name, name) => {
                return Task::done(Message::SetBusy(true))
                    .chain(Task::perform({
                        let name = name.clone();
                        async move {
                        if let Some(handle) = rfd::AsyncFileDialog::new()
                            .set_title(format!("Pick {name} Folder") )
                            .pick_folder()
                            .await
                        {
                            let path = handle.path();

                            let mut read_dir = path.read_dir()?;
                            eyre::ensure!(read_dir.next().is_some(), "The folder that was selected is empty, import request will be disregarded.");

                            match name.as_str() {
                                library::profile_folder::CHARACTER_POOL => {
                                    eyre::ensure!(path.join("DefaultCharacterPool.bin").try_exists()?, "The selected folder does not appear to contain a default character pool (missing 'DefaultCharacterPool.bin')");
                                }
                                // Realistically anything could go into the config folder, nothing to do
                                library::profile_folder::CONFIG => {}
                                library::profile_folder::PHOTOBOOTH => {
                                    eyre::ensure!(path.join("PhotoboothData.x2").try_exists()?, "The selected folder does not appear to contain any photobooth data (missing 'PhotoboothData.x2')");
                                }
                                library::profile_folder::SAVE_DATA => {
                                    eyre::ensure!(path.join("profile.bin").try_exists()?, "The selected folder does not appear to contain any save data (missing 'profile.bin')");
                                },
                                _ => {}
                            }

                            let profile_dir = PROFILES_DIR.join(&profile_name);
                            let dest = profile_dir.join(name);
                            if dest.try_exists()? {
                                tokio::fs::remove_dir_all(&dest).await?;
                            }
                            tokio::fs::create_dir_all(&dest).await?;
                            dircpy::copy_dir(path, &dest)?;

                            Ok(())
                        } else {
                            Ok(())
                        }
                    }}, move |res| {
                        if let Err(err) = res {
                            Message::display_error(format!("Error Importing {name}"), format!("{err:?}"))
                        } else {
                            Message::None
                        }
                    }))
                    .chain(Task::done(Message::SetBusy(false)));
            }
            Message::ProfileImportCollectionRequested(collection) => {
                let (dialog, mut receiver) =
                    AsyncDialog::builder(AsyncDialogKey::ProfileImportCollection, "Set Name", "")
                        .with_string_default("Name", &collection.title)
                        .finish();

                let used_names = self.profiles.keys().cloned().collect::<HashSet<_>>();

                return Task::batch([
                    Task::future(async move {
                        match receiver.next().await.flatten() {
                            Some(fields) => {
                                let name =
                                    fields.get_string("Name").unwrap_or_default().to_string();

                                if name.is_empty() {
                                    Message::display_error(
                                        "Empty Name",
                                        "Cannot create a profile with no name.",
                                    )
                                } else if used_names.contains(&name) {
                                    Message::display_error(
                                        "Name In Use",
                                        "There is already a profile with this name.",
                                    )
                                } else if let Err(message) = library::validate_profile_name(&name) {
                                    Message::display_error("Invalid Name", message)
                                } else {
                                    Message::Chained(vec![
                                        Message::ProfileAddEdited(name.clone()),
                                        Message::ProfileAddCompleted(true),
                                        Message::ProfileAddItems(
                                            name,
                                            collection.items.iter().map(ModId::from).collect(),
                                        ),
                                    ])
                                }
                            }
                            None => Message::None,
                        }
                    }),
                    Task::done(Message::OpenModal(Arc::new(AppModal::AsyncDialog(dialog)))),
                ]);
            }
            Message::ProfilePageResized(resize) => self.handle_profile_resize(resize),
            Message::ProfileViewDetails(name) => {
                self.modal_stack.push(AppModal::ProfileDetails(name));
            }
            Message::ProfileExportSnapshotRequested(name) => {
                if !self.save.tracked_profiles.contains(&name) {
                    return Task::none();
                }

                let source = PROFILES_DIR.join(&name);
                if !source.exists() {
                    return Task::done(Message::display_error(
                        "Profile Not Found",
                        format!("Failed to read profile directory for '{name}'."),
                    ));
                }

                let task = Task::future(async move {
                    let now = chrono::Utc::now();
                    if let Some(destination) = rfd::AsyncFileDialog::new()
                        .set_title("Pick Snapshot Output")
                        .add_filter("Zip File", &["zip"])
                        .set_directory(snapshot::SNAPSHOTS_DIR.as_path())
                        .set_file_name(format!("{name}_{}.zip", now.format("%Y.%m.%d_%Hh%Mm%Ss")))
                        .save_file()
                        .await
                        .map(|handle| {
                            if handle.path().extension().is_some_and(|ext| ext == "zip") {
                                handle.path().to_path_buf()
                            } else {
                                handle.path().with_added_extension("zip")
                            }
                        })
                    {
                        match snapshot::BasicSnapshotBuilder::new(source, destination.clone())
                            .comment(format!(
                                "Snapshot of '{name}' automatically generated at {now}."
                            ))
                            .finalize()
                        {
                            Err(err) => Message::display_error(
                                "Snapshot Failed",
                                eyre::Error::new(err)
                                    .wrap_err("Failed to load snapshot")
                                    .to_string(),
                            ),
                            Ok(name) => Message::display_error(
                                "Success",
                                format!(
                                    "Snapshot '{name}' successfully exported to '{}'",
                                    destination.display()
                                ),
                            ),
                        }
                    } else {
                        Message::None
                    }
                });

                return Task::done(Message::SetBusy(true))
                    .chain(task)
                    .chain(Task::done(Message::SetBusy(false)));
            }
            Message::ProfileImportSnapshotRequested => {
                let used_names = self.save.tracked_profiles.clone();
                let (dialog, mut receiver) = AsyncDialog::builder(
                    AsyncDialogKey::ProfileImportSnapshot,
                    "Pick Name",
                    "Pick a name for the imported profile",
                )
                .with_string("Profile Name")
                .finish();

                let task = Task::done(Message::SetBusy(true))
                    .chain(Task::done(Message::OpenModal(Arc::new(
                        AppModal::AsyncDialog(dialog),
                    ))))
                    .chain(Task::future(async move {
                        if let Some(response) = receiver.next().await.flatten() {
                            let name = response
                                .get_string("Profile Name")
                                .map(str::to_owned)
                                .unwrap_or_default();

                            if let Err(err) = library::validate_profile_name(&name) {
                                return Message::display_error("Invalid Profile Name", err);
                            };

                            if used_names.contains(&name) {
                                return Message::display_error(
                                    "Invalid Profile Name",
                                    format!("Profile with name '{name}' already exists."),
                                );
                            }

                            let Some(path) = rfd::AsyncFileDialog::new()
                                .set_title("Pick Snapshot")
                                .add_filter("Zip File", &["zip"])
                                .set_directory(snapshot::SNAPSHOTS_DIR.as_path())
                                .pick_file()
                                .await
                                .map(|handle| handle.path().to_path_buf())
                            else {
                                return Message::None;
                            };

                            let archive = match std::fs::File::open(&path)
                                .and_then(|f| ZipArchive::new(f).map_err(Into::into))
                            {
                                Err(err) => {
                                    let err = eyre::Error::new(err)
                                        .wrap_err("Could not read picked zip archive.");
                                    return Message::display_error(
                                        "Invalid Archive",
                                        format!("{err:?}"),
                                    );
                                }
                                Ok(archive) => archive,
                            };

                            match archive.snapshot_type() {
                                snapshot::SnapshotType::Invalid => Message::display_error(
                                    "Invalid Archive",
                                    "The picked zip archive was not an LXCOMM snapshot.",
                                ),
                                snapshot::SnapshotType::Basic => {
                                    Message::ProfileImportBasicSnapshot(name, Arc::new(archive))
                                }
                                snapshot::SnapshotType::Incremental => {
                                    Message::ProfileImportIncrementalSnapshot(
                                        name,
                                        Arc::new(archive),
                                    )
                                }
                            }
                        } else {
                            Message::None
                        }
                    }))
                    .chain(Task::done(Message::SetBusy(false)));

                return task;
            }
            Message::ProfileImportBasicSnapshot(name, archive) => {
                let Ok(archive) = Arc::try_unwrap(archive) else {
                    return Task::done(Message::display_error(
                        "Unexpected Error",
                        "Unexpected error occurred attempting to import snapshot. Please report this to the devs.",
                    ));
                };

                return Task::done(Message::busy_message("Importing snapshot..."))
                    .chain(Task::future(async move {
                        match tokio::task::spawn_blocking(move || {
                            snapshot::load_basic_snapshot(name.clone(), &PROFILES_DIR, archive)
                        })
                        .await
                        {
                            Err(err) => Message::display_error(
                                "Unexpected Error",
                                format!("Unexpected error while importing profile: {err:#?}"),
                            ),
                            Ok(Err(err)) => {
                                let err =
                                    eyre::Error::new(err).wrap_err("Failed to import snapshot");
                                Message::display_error("Error Importing", format!("{err:#?}"))
                            }
                            Ok(Ok(profile)) => Message::ProfileImportSnapshotCompleted(profile),
                        }
                    }))
                    .chain(Task::done(Message::SetBusy(false)));
            }
            Message::ProfileImportIncrementalSnapshot(name, archive) => {
                let Ok(mut archive) = Arc::try_unwrap(archive) else {
                    return Task::done(Message::display_error(
                        "Unexpected Error",
                        "Unexpected error occurred attempting to import snapshot. Please report this to the devs.",
                    ));
                };

                let timestamps = match snapshot::list_incremental_snapshots(&mut archive) {
                    Err(err) => {
                        let err = eyre::Error::new(err).wrap_err("Failed to read from archive.");
                        return Task::done(Message::display_error(
                            "Error Reading Archive",
                            format!("{err:#}"),
                        ));
                    }
                    Ok(list) => list,
                };

                let _ = self.update(Message::busy_message("Importing snapshot..."));

                let receiver = if timestamps.len() <= 1 {
                    None
                } else {
                    let (dialog, receiver) = AsyncDialog::builder(
                        AsyncDialogKey::ProfileImportSnapshot,
                        "Pick Snapshot Date",
                        "Pick date to load snapshot from.",
                    )
                    .with_string_enum(
                        "Timestamp",
                        timestamps.iter().filter_map(|i| {
                            let date_time = chrono::DateTime::from_timestamp(*i, 0)?
                                .with_timezone(&chrono::Local);
                            Some(format!("{}", date_time.format("%c")))
                        }),
                    )
                    .finish();
                    self.modal_stack.push(AppModal::AsyncDialog(dialog));
                    Some(receiver)
                };

                return Task::future(async move {
                    let stop_at = if let Some(mut receiver) = receiver {
                        let Some(response) = receiver.next().await.flatten() else {
                            return Message::None;
                        };
                        response
                            .get_string_enum("Timestamp")
                            .and_then(|formatted| {
                                chrono::NaiveDateTime::parse_from_str(formatted, "%c").ok()
                            })
                            .and_then(|dt| {
                                dt.and_local_timezone(chrono::Local)
                                    .single()
                                    .map(|dt| dt.timestamp())
                            })
                    } else {
                        None
                    };

                    dbg!(stop_at);

                    match tokio::task::spawn_blocking(move || {
                        snapshot::load_incremental_snapshot(
                            name.clone(),
                            &PROFILES_DIR,
                            archive,
                            stop_at,
                        )
                    })
                    .await
                    {
                        Err(err) => Message::display_error(
                            "Unexpected Error",
                            format!("Unexpected error while importing profile: {err:#?}"),
                        ),
                        Ok(Err(err)) => {
                            let err = eyre::Error::new(err).wrap_err("Failed to import snapshot");
                            Message::display_error("Error Importing", format!("{err:#?}"))
                        }
                        Ok(Ok(profile)) => Message::ProfileImportSnapshotCompleted(profile),
                    }
                })
                .chain(Task::done(Message::SetBusy(false)));
            }
            Message::ProfileImportSnapshotCompleted(mut profile) => {
                profile.settings_editing = profile.settings.clone();
                self.save.tracked_profiles.insert(profile.name());
                self.profiles.insert(profile.name(), profile);
            }
            Message::ActiveProfileSelected(name) => {
                if self.profiles.contains_key(&name) {
                    self.save.active_profile = Some(name);
                }
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
                let xcom_subdir = if self.save.linux_native_mode {
                    "xcomgame"
                } else {
                    "XComGame"
                };
                if !destination.join(xcom_subdir).exists() {
                    return Task::done(Message::display_error(
                        "Invalid Game Directory",
                        format!(
                            "Unable to find the '{xcom_subdir}' subdirectory in {}",
                            destination.display()
                        ),
                    ));
                }

                if let Some(active) = &self.save.active_profile
                    && let Some(profile) = self.profiles.get(active)
                {
                    if let Err(err) = loading::bootstrap_load_profile(
                        profile,
                        loading::LoadSettings {
                            linux_native_mode: self.save.linux_native_mode,
                        },
                        &self.settings.download_directory,
                        self.file_cache.get_mod_metadata_list(),
                        destination,
                        local_path,
                    ) {
                        return Task::done(Message::display_error(
                            "Error Applying Config",
                            format!("{err:#?}"),
                        ));
                    } else if manual {
                        return if profile.settings.automatic_snapshot_on_apply {
                            Task::done(Message::SetBusyMessage(
                                "Generating Automatic Snapshot...".to_string(),
                            ))
                            .chain(Task::perform(
                                profile.generate_automatic_snapshot(),
                                |res| {
                                    if let Err(err) = res {
                                        let err = eyre::Error::new(err)
                                            .wrap_err("Failed to create an automatic snapshot.");
                                        Message::display_error(
                                            "Error Generating Snapshot",
                                            format!("{err:?}"),
                                        )
                                    } else {
                                        Message::display_error(
                                            "Success",
                                            "Config was successfully applied.",
                                        )
                                    }
                                },
                            ))
                            .chain(Task::done(Message::SetBusy(false)))
                        } else {
                            // TODO - More generic dialog name, just using error for now
                            Task::done(Message::display_error(
                                "Success",
                                "Config was successfully applied.",
                            ))
                        };
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
            Message::LoadToggleLinuxNative(toggled) => {
                self.save.linux_native_mode = toggled;
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

                    let generate = self
                        .save
                        .active_profile
                        .as_ref()
                        .and_then(|name| self.profiles.get(name))
                        .filter(|profile| profile.settings.automatic_snapshot_on_launch)
                        .map(|profile| profile.generate_automatic_snapshot());

                    let _ = if generate.is_some() {
                        self.update(Message::SetBusyMessage(
                            "Generating Automatic Snapshot...".to_string(),
                        ))
                    } else {
                        self.update(Message::SetBusy(true))
                    };

                    Task::future(async move {
                        let io_result: std::io::Result<()> = try {
                            if let Some(generate) = generate {
                                generate.await?;
                            }

                            let mut command = std::process::Command::new(command);
                            command.stdout(Stdio::null());

                            for (l, r) in args {
                                if !l.is_empty() {
                                    command.arg(l);
                                }
                                command.arg(r);
                            }
                            command.spawn()?;
                        };

                        if let Err(err) = io_result {
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
                    })
                    .chain(self.setup_launch_log_monitor())
                } else {
                    Task::done(Message::DisplayError(
                        "Missing Command".to_string(),
                        "No command has been set.".to_string(),
                    ))
                };
            }

            Message::LoadFindGameRequested => {
                let search = if self.save.linux_native_mode {
                    "XCOM 2/share/data/binaries"
                } else {
                    "XCOM 2/XCom2-WarOfTheChosen/Binaries"
                };
                let (task, abort) = files::find_directories_matching(search);

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
                let (modal, mut rec) = AppModal::async_choose(
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
                let search = if self.save.linux_native_mode {
                    "feral-interactive/XCOM 2 WotC/VFS/Local/my games/XCOM2 War of the Chosen/XComGame"
                } else {
                    "Documents/My Games/XCOM2 War of the Chosen/XComGame"
                };
                let (task, abort) = files::find_directories_matching(search);

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
                let (modal, mut rec) = AppModal::async_choose(
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
                            dependencies.iter().map(ModId::from).collect(),
                        ),
                        Err(err) => Message::display_error(
                            "Error Resolving Dependencies",
                            format!("An error occurred while resolving dependencies:\n{err:?}"),
                        ),
                    }
                }))
                .chain(Task::done(Message::SetBusy(false)));
            }

            #[cfg(feature = "dbus")]
            Message::DbusError(err) => {
                eprintln!("Unrecoverable dbus error: {err:?}");
                return iced::window::oldest().and_then(iced::window::close);
            }

            Message::GainFocus => {
                return iced::window::oldest().and_then(|id| {
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

                let logout_task = if self.settings.steamcmd_logout_on_exit
                    && let Some(session) = self.steamcmd_state.session.clone()
                    && session.is_logged_in()
                {
                    Task::future(async move {
                        if session.is_waiting()
                            && let Err(err) = session.log_out().await
                        {
                            eprintln!("Error logging out of SteamCMD: {err:?}");
                        }
                        Message::None
                    })
                } else {
                    Task::none()
                };

                let quit_task = if let Some(session) = self.steamcmd_state.session.clone()
                    && !session.is_killed()
                {
                    Task::future(async move {
                        if session.is_waiting() {
                            if let Err(err) = session.quit().await {
                                eprintln!("Error exiting SteamCMD session: {err:?}");
                            }
                        } else {
                            eprintln!(
                                "Could not exit SteamCMD cleanly, an operation is still ongoing..."
                            )
                        }
                        Message::None
                    })
                } else {
                    Task::none()
                };

                let result: std::io::Result<()> = try {
                    let file = std::fs::File::create(&*SAVE_PATH)?;
                    serde_json::to_writer_pretty(file, &self.save).map_err(std::io::Error::from)?;
                };
                if let Err(err) = result {
                    eprintln!("Error saving application data: {err:?}");
                }

                return Task::done(Message::SetBusyMessage("Shutting down...".to_string()))
                    .chain(logout_task)
                    .chain(quit_task)
                    .chain(iced::window::oldest().and_then(iced::window::close));
            }
            Message::LoggingSetup(sender) => {
                println!("Logging should be set up...");
                let _ = Arc::get_mut(&mut self.steamcmd_state)
                    .expect(
                        "application should currently have exclusive ownership of steamcmd state",
                    )
                    .message_sender
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
                AppSettingEdit::String(name, new) => {
                    apply!(name, String, new)
                }
                AppSettingEdit::Bool(name, new) => apply!(name, bool, new),
                AppSettingEdit::Number(name, new) => apply!(name, u32, new),
            },
            SettingsMessage::ResetToDefault => {
                self.settings_editing = AppSettings {
                    // Potential TODO - add attribute for skipping values that should not be reset to "default"
                    steam_webapi_api_key: self.settings_editing.steam_webapi_api_key.clone(),
                    ..AppSettings::default()
                }
            }
            SettingsMessage::ResetToSaved => self.settings_editing = self.settings.clone(),
            SettingsMessage::Save => {
                let old = self.settings.clone();
                self.settings = self.settings_editing.clone();

                if old.download_directory != self.settings.download_directory {
                    self.scan_downloads();
                }

                if old.steam_webapi_api_key != self.settings.steam_webapi_api_key {
                    self.api_key = SecretString::from(self.settings.steam_webapi_api_key.as_str());
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

                let result: std::io::Result<()> = try {
                    std::fs::write(
                        &*SETTINGS_PATH,
                        serde_json::to_string_pretty(&self.settings)
                            .map_err(std::io::Error::from)?,
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

            SettingsMessage::AddModDirectory => {
                return Task::done(Message::SetBusy(true))
                    .chain(Task::future(async move {
                        if let Some(handle) = rfd::AsyncFileDialog::new()
                            .set_title("Pick Local Mod Directory")
                            .pick_folder()
                            .await
                        {
                            SettingsMessage::AddModDirectoryConfirmed(handle.path().to_path_buf())
                                .into()
                        } else {
                            Message::None
                        }
                    }))
                    .chain(Task::done(Message::SetBusy(false)));
            }
            SettingsMessage::AddModDirectoryConfirmed(path) => {
                if self.save.local_mod_dirs.insert(path.clone()) {
                    self.scan_mods(&path, false);
                }
            }
            SettingsMessage::RemoveModDirectory(index) => {
                if let Some(path) = self.save.local_mod_dirs.shift_remove_index(index) {
                    let res: std::io::Result<()> = try {
                        for entry in path.read_dir()? {
                            let entry = entry?;
                            let id = ModId::Local(entry.path());
                            self.library.items.remove(&id);
                            self.local.remove(&entry.path());
                            self.file_cache.invalidate_details(&id);
                        }
                    };
                    if let Err(err) = res {
                        eprintln!(
                            "Error attempting to clear local mod information from session: {err:?}"
                        );
                    }
                }
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

    fn view_profile_mod_list<'a>(
        &'a self,
        profile: &'a library::Profile,
    ) -> widget::Column<'a, Message> {
        column![
            row!(
                button("View Details")
                    .on_press_with(|| Message::ProfileViewDetails(profile.name()))
            )
            .extend(library::profile_folder::ALL.iter().map(|name| {
                button(text!("View {name}"))
                    .on_press_with(|| {
                        Message::ProfileViewFolderRequested(profile.name(), name.to_string())
                    })
                    .into()
            }))
            .extend(library::profile_folder::ALL.iter().map(|name| {
                button(text!("Import {name}"))
                    .on_press_with(|| {
                        Message::ProfileImportFolderRequested(profile.name(), name.to_string())
                    })
                    .style(button::secondary)
                    .into()
            }))
            .push(
                button("Export Snapshot")
                    .style(button::success)
                    .on_press_with(|| Message::ProfileExportSnapshotRequested(profile.name()))
            )
            .push(
                button("Delete Profile")
                    .style(button::danger)
                    .on_press_with(|| Message::ProfileDeletePressed(profile.name())),
            )
            .wrap(),
            text("Mods").size(20),
            scrollable(column(profile.items.keys().map(|id| {
                let mut issues = vec![];
                let missing = if let ModId::Workshop(id) = id
                    && !self.item_downloaded(*id)
                {
                    issues.push("Missing: Mod is not downloaded (or found)".to_string());
                    true
                } else if let ModId::Local(path) = id
                    && !path.exists()
                {
                    issues.push(format!("Missing: Could find mod at {}", path.display()));
                    true
                } else {
                    false
                };

                issues.extend(
                    profile
                        .compatibility_issues
                        .get(id)
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                        .iter()
                        .map(|item| match item {
                            library::CompatibilityIssue::MissingWorkshop(_id, message) => {
                                format!("Missing Workshop Dependency: {message}")
                            }
                            library::CompatibilityIssue::MissingRequired(dlc_name) => {
                                format!("Missing Dependency: {dlc_name}")
                            }
                            library::CompatibilityIssue::Incompatible(dlc_name) => {
                                format!("Incompatible Mod: {dlc_name}")
                            }
                            library::CompatibilityIssue::Overlapping(name, provides) => {
                                format!(
                                    "{name} Provides Same DLCNames: {}",
                                    provides.iter().join(", ")
                                )
                            }
                            library::CompatibilityIssue::Unknown => "Missing Info".to_string(),
                        }),
                );

                let button_style = if let Some(sel_id) = &profile.view_selected_item
                    && sel_id == id
                {
                    button::secondary
                } else if !issues.is_empty() {
                    button::danger
                } else {
                    button::primary
                };
                let missing_text = if missing { " (MISSING)" } else { "" };
                let select = if let Some(details) = self.file_cache.get_details(id) {
                    button(text!("{} ({id}){missing_text}", details.title()))
                        .on_press_with(|| Message::ProfileItemSelected(id.clone()))
                } else {
                    button(text!("UNKNOWN ({id}){missing_text}"))
                }
                .style(button_style)
                .width(Fill);

                if !issues.is_empty() {
                    row![
                        button("X").style(button::danger).on_press_with(|| {
                            Message::ProfileRemoveItems(profile.name(), vec![id.clone()])
                        }),
                        tooltip!(
                            select,
                            column(issues.into_iter().map(|s| text(s).into())),
                            tooltip::Position::Bottom,
                        )
                    ]
                    .into()
                } else {
                    select.into()
                }
            })))
        ]
    }

    fn view_profile_mod_editor<'a>(
        &'a self,
        profile: &'a library::Profile,
        item_id: &'a ModId,
    ) -> widget::Column<'a, Message> {
        column![
            row![
                button("View Details").on_press_with(|| Message::SetViewingItem(item_id.clone())),
                space::horizontal(),
                button("Remove Mod")
                    .style(button::danger)
                    .on_press_with(|| Message::ProfileRemoveItems(
                        profile.name(),
                        vec![item_id.clone()]
                    ))
            ],
            self.mod_editor.view(self)
        ]
    }

    fn view_item_detailed<'a>(&'a self, id: &'a ModId) -> Element<'a, Message> {
        let Some(file) = self.file_cache.get_details(id) else {
            return container(
                column![
                    text("Failed to load details"),
                    space::vertical(),
                    row![
                        space::horizontal(),
                        button("Close")
                            .style(button::danger)
                            .on_press(Message::CloseModal)
                    ],
                ]
                .height(Fill)
                .width(Fill),
            )
            .style(container::rounded_box)
            .into();
        };

        let download_button = if self.item_downloaded(id.as_u32()) {
            button("Update")
        } else {
            button("Download")
        }
        .on_press_maybe(id.maybe_workshop().and_then(|id| {
            self.is_downloading(id)
                .not()
                .then_some(Message::SteamCMDDownloadRequested(id))
        }));

        let tags = if let Some(item) = self.library.items.get(id) {
            let metadata = self.file_cache.get_metadata(&item.path);
            println!("Metadata: {metadata:?}");
            &mut metadata.tags.into_iter() as &mut dyn Iterator<Item = metadata::Tag>
        } else {
            &mut file
                .tags()
                .iter()
                .map(|tag| metadata::Tag::from(tag.tag.as_str()))
                as &mut dyn Iterator<Item = metadata::Tag>
        };
        let tags = tags
            .map(|tag| {
                println!("Tag: {tag}");
                Element::new(iced_aw::badge(text(tag.to_string())))
            })
            .collect::<Vec<_>>();

        container(
            container(column![
                row![
                    web::image_from_source(
                        &self.images,
                        if let ModDetails::Workshop(details) = &file {
                            ImageSource::Web(details.preview_url.clone())
                        } else if let Some(path) = id.maybe_local()
                            && path.join("ModPreview.jpg").exists()
                        {
                            ImageSource::Path(path.join("ModPreview.jpg"))
                        } else {
                            ImageSource::Web(String::new())
                        }
                    )
                    .height(256)
                    .width(256)
                    .padding(16),
                    column![
                        text(file.title().to_owned()).size(18),
                        file.maybe_workshop().map(|file| {
                            tooltip!(rich_text([text::Span::new("Author: "), text::Span::new(web::get_user_display_name(steam_rs::Steam::new(self.api_key.expose_secret()), file.creator.0)).link(file.creator.0)]).on_link_click(|id: u64| {
                                web::open_browser(format!("https://steamcommunity.com/profiles/{id}/myworkshopfiles/?appid={XCOM_APPID}"))
                            }), "Open in Browser")
                        }),
                        id.maybe_workshop().map(|id| {
                            tooltip!(
                                rich_text([text::Span::new(id.to_string()).link(id)]).on_link_click(|id: u32| {
                                    web::open_browser(format!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}"))
                                }),
                                "Open in Browser",
                            )
                        }),
                        text!("{:.2} out of 10", file.get_score() * 10.0),
                    ]
                    .push(file.children().is_empty().not().then(|| {
                        row!["Dependencies -"]
                            .extend(file.children().iter().map(|child| {
                                let child_id = child
                                    .published_file_id
                                    .parse::<u32>()
                                    .expect("id should be a valid id");
                                if let Some(details) =
                                    self.file_cache.get_details(ModId::from(child_id))
                                {
                                    row![
                                        rich_text([span(format!(
                                            "{} ({child_id}) ",
                                            details.title()
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
                        button("Add to Profile").on_press_with(|| {
                            Message::ItemDetailsAddToLibraryRequest(vec![id.clone()])
                        }),
                    ])
                    .push(id.maybe_workshop().map(|id| row![
                        file.children().is_empty().not().then_some(
                            button("Download All Dependencies")
                                .on_press(Message::DownloadAllRequested(id))
                        ),
                        button("Add All Dependencies to Profile")
                            .on_press(Message::ItemDetailsAddToLibraryAllRequest(id))
                    ]))
                    .push(id.maybe_workshop().and_then(|id| {
                        self.item_downloaded(id).then_some(
                            button("Delete")
                                .style(button::danger)
                                .on_press(Message::DeleteRequested(id)),
                        )
                    }),)
                    .push(
                        tags.is_empty()
                            .not()
                            .then_some(column![text("Tags"), row(tags).spacing(8).wrap()])
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
                            .map(web::handle_url),
                        )
                        .width(Fill)
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

    fn async_choose_modal<'a>(&'a self, dialog: &'a AppModal) -> Element<'a, Message> {
        let AppModal::AsyncChoose {
            key,
            title,
            body,
            sender: _,
            options,
            strategy: _,
        } = dialog
        else {
            panic!("async_choose_modal should only be passed an async choose modal")
        };

        iced_aw::card(text(title).width(Fill), column![scrollable(text(body)),])
            .foot(row(options.iter().enumerate().map(|(i, display)| {
                button(text(display))
                    .on_press_with(move || Message::AsyncChooseResolve(key.clone(), i))
                    .into()
            })))
            .max_height(512.0)
            .max_width(512.0)
            .width(Fill)
            .into()
    }

    fn settings_page(&self) -> Element<'_, Message> {
        let settings = &self.settings_editing;
        let col = column(APP_SETTINGS_INFO.iter().map(|field| {
            let name = field.name();

            let label = app_field_label(field);

            let can_edit = !(cfg!(feature = "flatpak")
                && field.get_attribute::<AppSettingsFlatpakLocked>().is_some());

            let editor = match field
                .get_attribute::<AppSettingEditor>()
                .unwrap_or_else(|| {
                    AppSettingEditor::get_default_for(
                        field
                            .type_info()
                            .expect("field should not be a dynamic type"),
                    )
                }) {
                editor @ (AppSettingEditor::StringInput
                | AppSettingEditor::SecretInput
                | AppSettingEditor::StringPick(_)) => container(
                    editor.render(
                        field,
                        name,
                        settings
                            .get_field(name)
                            .expect("field should contain a string value"),
                        can_edit,
                        AppSettingEdit::String,
                    ),
                )
                .width(600)
                .into(),
                editor @ AppSettingEditor::BoolToggle => editor.render(
                    field,
                    name,
                    settings
                        .get_field::<bool>(name)
                        .expect("field should contain a boolean"),
                    can_edit,
                    AppSettingEdit::Bool,
                ),
                editor @ AppSettingEditor::NumberInput(_) => editor.render(
                    field,
                    name,
                    settings
                        .get_field::<u32>(name)
                        .expect("field should be a u32"),
                    can_edit,
                    AppSettingEdit::Number,
                ),
            };

            row([label.into(), space::horizontal().into(), editor])
                .height(32)
                .into()
        }))
        .push(rule::horizontal(2))
        .push(text("Local Mod Directories"))
        .extend(
            self.save
                .local_mod_dirs
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    row![
                        text_input("", &path.display().to_string()).on_input(|_| Message::None),
                        button("X")
                            .style(button::danger)
                            .on_press(SettingsMessage::RemoveModDirectory(index).into())
                    ]
                    .into()
                }),
        )
        .push(button("Add Directory").on_press(SettingsMessage::AddModDirectory.into()));

        let scroll = scrollable(col.padding(16)).height(Fill);

        let bottom = row![
            space::horizontal(),
            button("Reset to Default").on_press(SettingsMessage::ResetToDefault.into()),
            button("Reset to Saved").on_press(SettingsMessage::ResetToSaved.into()),
            button("Save").on_press_maybe(
                (self.settings != self.settings_editing).then_some(SettingsMessage::Save.into())
            )
        ];

        column![scroll, bottom].into()
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
                    AppPage::Browse => self.render_browser(self, self.loaded_files.iter()),
                    AppPage::SteamCMD => self.steamcmd_page(),
                    AppPage::Settings => self.settings_page(),
                    AppPage::Downloads => self.downloads_page(),
                    AppPage::GameLogs => self.game_logs_page(),
                    AppPage::Collections => self.collections.render_browser(self, self.collections.loaded_web_collections.iter()),
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
                        AppModal::ProfileDetails(name) => {
                            container(container(column![
                                row![container(text("Profile Details").size(24)).padding(4), space::horizontal(), button("X").style(button::danger).on_press_with(|| {
                                    if let Some(profile) = self.profiles.get(name) && profile.settings != profile.settings_editing {
                                        Message::display_error("Unsaved Changes", "This profile has unsaved changes!")
                                    } else {
                                        Message::CloseModal
                                    }
                                })],
                                if let Some(profile) = self.profiles.get(name) {
                                    profile.view_details(&self.library, &self.file_cache)
                                } else {
                                    text("Non-existent profile...?").into()
                                }
                            ]).style(container::rounded_box).height(Fill).width(Fill)).padding(16).into()
                        }
                        AppModal::AddToProfileRequest(ids) => self.add_to_profile_modal(ids),
                        AppModal::ItemDetailedView(id) => self.view_item_detailed(id),
                        AppModal::CollectionDetailedView(source) => self.collections.view_collection_detailed(self, source),
                        AppModal::AsyncDialog(dialog) => dialog.view().max_width(512.0).into(),
                        dialog @ AppModal::AsyncChoose {
                            strategy: AsyncDialogStrategy::Stack | AsyncDialogStrategy::Replace,
                            ..
                        } => self.async_choose_modal(dialog),
                        dialog @ AppModal::AsyncChoose {
                            strategy: AsyncDialogStrategy::Queue,
                            key: current_key,
                            ..
                        } => {
                            if self
                                .modal_stack
                                .iter()
                                .take_while(|other| other != &modal)
                                .any(|other| {
                                    matches!(other, AppModal::AsyncChoose { key, .. } if key == current_key)
                                })
                            {
                                return None;
                            }

                            self.async_choose_modal(dialog)
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
                        self.save.active_profile.as_ref(),
                        Message::ActiveProfileSelected,
                    )
                ).width(Fill),
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
                        .on_input_maybe(cfg!(not(feature = "flatpak")).then_some(|s| Message::LoadSetGameDirectory(PathBuf::from(s)))),
                    ],
                ).width(Fill),
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
                            )
                            .on_input_maybe(cfg!(not(feature = "flatpak")).then_some(|s| Message::LoadSetLocalDirectory(PathBuf::from(s)))),
                            container("This is the path where the game expects find your local data (e.g. saves). Files/folders that are modified will have backups automatically created."),
                        ),
                    ],
                ).width(Fill),
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
                ).width(Fill),
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
                ).width(Fill),
                tooltip!(
                    toggler(self.save.linux_native_mode)
                        .label("Linux Native Mode")
                        .on_toggle_maybe(cfg!(target_os = "linux").then_some(Message::LoadToggleLinuxNative)),
                    container("Enable if running the game natively on Linux instead of through Proton/WINE")
                ),
                row![
                    button("Apply").on_press_maybe(self.save.active_profile.is_some().then_some(Message::LoadPrepareProfile(true))),
                    button("Launch").on_press_maybe(self.save.active_profile.is_some().then_some(Message::LoadLaunchGame)),
                ].spacing(8),
            ]
            .width(Fill)
            .padding(16),
        )
    }

    fn library_page(&self) -> Element<'_, Message> {
        let all_selected = self.library.iter_filtered().all(|item| item.selected);
        let some_selected = self.library.iter_filtered().any(|item| item.selected);

        #[derive(Debug, Clone, Copy)]
        struct ItemMeta<'a> {
            id: &'a ModId,
            missing: Option<&'a Vec<String>>,
            text_style: fn(&Theme) -> iced::widget::text::Style,
        }

        let cols = [
            table::column(
                checkbox(all_selected).on_toggle(Message::LibraryToggleAll),
                |(item, _meta): (&LibraryItem, ItemMeta)| {
                    checkbox(item.selected).on_toggle(move |toggle| {
                        Message::LibraryToggleItem(item.id.clone(), toggle)
                    })
                },
            )
            .align_y(Center),
            table::column("", |(item, _meta): (&LibraryItem, ItemMeta)| {
                button(symbols::eye()).on_press_with(|| Message::SetViewingItem(item.id.clone()))
            })
            .align_y(Center),
            table::column("Workshop ID", |(_item, meta): (&LibraryItem, ItemMeta)| {
                let ItemMeta {
                    id,
                    missing,
                    text_style,
                } = meta;
                tooltip(
                    rich_text([span(id.get_hash().truncated_overflow(12).to_string())
                        .link(id.get_hash())
                        .underline(missing.is_some())])
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
                )
            })
            .align_y(Center),
            table::column("Title", |(item, meta): (&LibraryItem, ItemMeta)| {
                text(&item.title).style(meta.text_style)
            })
            .align_y(Center),
            table::column("ID", |(_item, meta): (&LibraryItem, ItemMeta)| {
                text(
                    self.file_cache
                        .get_mod_metadata(meta.id)
                        .map(|data| data.dlc_name.clone())
                        .unwrap_or("UNKNOWN".to_string()),
                )
                .style(meta.text_style)
            })
            .align_y(Center),
            table::column("Path", |(item, meta): (&LibraryItem, ItemMeta)| {
                text(item.path.display().to_string()).style(meta.text_style)
            })
            .align_y(Center),
        ];

        let table = iced::widget::table(
            cols,
            self.library.iter_filtered().map(|item| {
                let id = &item.id;
                let missing = self.library.missing_dependencies.get(id);
                let text_style = if missing.is_some() {
                    text::danger
                } else {
                    text::default
                };
                (
                    item,
                    ItemMeta {
                        id,
                        missing,
                        text_style,
                    },
                )
            }),
        );

        column![
            row![
                row![button("Rescan").on_press(Message::LibraryScanRequest)],
                rule::vertical(2),
                row![
                    button("Check Updates").on_press_maybe(
                        some_selected.then_some(Message::LibraryCheckOutdatedRequest)
                    ),
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
                // scrollable(container(table).padding(16))
                scrollable(table)
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

    fn steamcmd_page(&self) -> Element<'_, Message> {
        let state = &self.steamcmd_state;
        let settings = &self.steamcmd_state;
        let can_log_in = !(state.is_logged_in()
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
                    .highlight("log", iced::highlighter::Theme::SolarizedDark)
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
        self.ongoing_download
            .is_some_and(|(current, _)| current == id)
            || self.download_queue.contains_key(&id)
    }

    fn downloads_page(&self) -> Element<'_, Message> {
        macro_rules! view_details {
            ($id:expr) => {
                button("View Details").on_press(Message::SetViewingItem(ModId::Workshop($id)))
            };
        }

        let get_details = |(id, size): (&u32, &u64)| -> Element<'_, Message> {
            let cancel = self
                .download_queue
                .iter()
                .any(|(this, _)| this == id)
                .then_some(
                    column![
                        space::vertical(),
                        button("Cancel")
                            .style(button::danger)
                            .on_press(Message::DownloadCancelRequested(*id)),
                    ]
                    .height(50),
                );

            if let Some(ModDetails::Workshop(details)) =
                self.file_cache.get_details(ModId::from(id))
            {
                let total_size = details.file_size.parse::<u64>().ok().unwrap_or_default();
                let percent = *size as f32 / total_size as f32 * 100.0;
                let total_display = files::SizeDisplay::automatic(total_size);
                let size_display = files::SizeDisplay::automatic_from(*size, total_size);
                row![
                    column![
                        text!(
                            "{} ({id}) - {size_display} of {total_display}",
                            details.title
                        )
                        .shaping(text::Shaping::Advanced),
                        stack![
                            progress_bar(0.0..=total_size as f32, *size as f32),
                            container(text!("{percent:.2}%").align_x(Center).align_y(Center))
                                .center(Fill)
                        ],
                    ]
                    .width(Fill),
                ]
            } else {
                let displayed_size = files::SizeDisplay::automatic(*size);
                row![
                    text!("Unknown ({id}) - {displayed_size}"),
                    space::horizontal(),
                ]
            }
            .push(column![space::vertical(), view_details!(*id)].height(50))
            .push(cancel)
            .into()
        };

        let mut col = column(None).spacing(8);

        let ongoing_empty = self.ongoing_download.is_none();
        let queue_empty = self.download_queue.is_empty();
        let completed_empty = self.completed_downloads.is_empty();
        let errored_empty = self.errorred_downloads.is_empty();
        let pending_empty = self.pending_queue.is_empty();

        if !ongoing_empty {
            col = col.push(text("Ongoing Downloads").size(24));
            col = col.extend(
                self.ongoing_download
                    .iter()
                    .map(|(id, prog)| get_details((id, prog))),
            )
        }

        if !queue_empty {
            col = col.push(text("Queued Downloads").size(24));
            if !self.steamcmd_state.is_logged_in() {
                col = col.push(text("Note: You are not currently logged into SteamCMD"))
            }
            col = col.extend(self.download_queue.iter().map(get_details));
        }

        if !completed_empty {
            col = col.push(
                row![
                    text("Completed").size(24),
                    space::horizontal(),
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
                        let info =
                            if let Some(details) = self.file_cache.get_details(ModId::from(id)) {
                                format!("{} ({id})", details.title())
                            } else {
                                format!("Unknown ({id})")
                            };
                        row![
                            text(info).shaping(text::Shaping::Advanced),
                            space::horizontal(),
                            view_details!(*id),
                            button("Clear")
                                .on_press(Message::SteamCMDDownloadCompletedClear(vec![*id])),
                        ]
                        .into()
                    }),
            )
        }

        if !pending_empty {
            col = col.push(
                row![
                    text("Pending Updates").size(24),
                    space::horizontal(),
                    button("Download All").on_press_with(|| Message::DownloadPushPending(
                        self.pending_queue.keys().copied().collect_vec()
                    ))
                ]
                .align_y(Vertical::Bottom),
            );
            col = col.extend(self.pending_queue.iter().map(|(&id, &time)| {
                let info = if let Some(details) = self.file_cache.get_details(ModId::from(id)) {
                    format!("{} ({id})", details.title())
                } else {
                    format!("Unknown ({id})")
                };
                let time = chrono::DateTime::from_timestamp(time as i64, 0).unwrap_or_default();
                let formatted = time.naive_local().format("%Y %B %e - %I:%M%p").to_string();
                row![
                    column![
                        text(info).shaping(text::Shaping::Advanced),
                        text!("Last Updated - {formatted}")
                    ],
                    space::horizontal(),
                    button("View Change Notes").on_press_with(move || web::open_browser(format!(
                        "https://steamcommunity.com/sharedfiles/filedetails/changelog/{id}"
                    ))),
                    view_details!(id),
                    button("Download").on_press(Message::DownloadPushPending(vec![id]))
                ]
                .into()
            }))
        }

        if !errored_empty {
            col = col.push(
                row![
                    text("Errored Downloads").size(24),
                    space::horizontal(),
                    button("Clear All").on_press_with(|| Message::SteamCMDDownloadErrorClear(
                        self.errorred_downloads.keys().copied().collect()
                    ))
                ]
                .align_y(Vertical::Bottom),
            );
            col = col.extend(self.errorred_downloads.iter().map(
                |(id, reason)| -> Element<'_, Message> {
                    let info = if let Some(details) = self.file_cache.get_details(ModId::from(id)) {
                        format!("{} ({id})\n{reason}", details.title())
                    } else {
                        format!("Unknown ({id})\n{reason}")
                    };

                    row![
                        text(info),
                        space::horizontal(),
                        view_details!(*id),
                        button("Retry").on_press(Message::SteamCMDDownloadRequested(*id)),
                        button("Clear").on_press(Message::SteamCMDDownloadErrorClear(vec![*id])),
                    ]
                    .into()
                },
            ));
        }

        if ongoing_empty && queue_empty && completed_empty && errored_empty && pending_empty {
            col = col.push(text("No ongoing downloads..."))
        }

        scrollable(col).height(Fill).into()
    }

    fn game_logs_page(&self) -> Element<'_, Message> {
        column![
            button("Clear").on_press(Message::LaunchLogClear),
            container(
                text_editor(&self.launch_log)
                    .placeholder("Log currently empty...")
                    .on_action(Message::LaunchLogAction)
                    .font(iced::Font::MONOSPACE)
                    .highlight("log", iced::highlighter::Theme::SolarizedDark)
                    .height(Fill),
            )
            .style(container::dark)
            .width(Fill)
        ]
        .into()
    }

    fn queue_downloads(&mut self) -> Task<Message> {
        if !self.steamcmd_state.is_logged_in() || self.ongoing_download.is_some() {
            return Task::none();
        }

        if let Some((id, _)) = self.download_queue.pop_front() {
            self.perform_download(id)
        } else {
            Task::none()
        }
    }

    fn perform_download(&mut self, id: u32) -> Task<Message> {
        self.ongoing_download = Some((id, 0));
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
            space::horizontal(),
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

    fn add_to_profile_modal<'a>(&'a self, items: &'a [ModId]) -> Element<'a, Message> {
        let cols = [
            table::column(
                checkbox(self.profiles.values().all(|profile| profile.add_selected))
                    .on_toggle(Message::LibraryAddToProfileToggleAll),
                |(name, profile): (&String, &library::Profile)| {
                    checkbox(profile.add_selected).on_toggle(|toggle| {
                        Message::LibraryAddToProfileToggled(name.clone(), toggle)
                    })
                },
            ),
            table::column(
                "Profile",
                |(_name, profile): (&String, &library::Profile)| text(profile.name.as_str()),
            ),
        ];

        iced_aw::card(
            "Add to Profile",
            scrollable(table::table(cols, self.profiles.iter())),
        )
        .foot(row![
            button("Cancel")
                .style(button::danger)
                .on_press(Message::LibraryAddToProfileConfirm(Vec::new())),
            space::horizontal(),
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
                space::horizontal(),
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
            space::horizontal(),
            button("Delete")
                .style(button::danger)
                .on_press(Message::LibraryDeleteConfirm),
        ])
        .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::Subscription::batch([
            #[cfg(feature = "dbus")]
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

    fn scan_downloads(&mut self) {
        self.scan_mods(
            &files::get_all_items_directory(&self.settings.download_directory),
            true,
        );
    }

    // Potential TODO: If this ends up being an expensive operation on large mod libraries,
    // add option to skip checking already included files or turn it into a stream
    fn scan_mods(&mut self, directory: &Path, is_workshop: bool) {
        // self.downloaded.clear();
        // let dir = files::get_all_items_directory(&self.settings.download_directory);
        // Assume fresh state where nothing has been downloaded before
        if !directory.exists() {
            return;
        }

        match std::fs::read_dir(directory) {
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

                                        if is_workshop
                                            && *file_name != *data.published_file_id.to_string()
                                        {
                                            data.published_file_id = file_name.to_string_lossy().parse::<u32>()
                                                .expect("steam workshop items should be contained in a folder named by its ID");
                                        }

                                        // Heuristically override description with readme for local files
                                        if !is_workshop
                                            && let Ok(description) =
                                                std::fs::read_to_string(entry.join("ReadMe.txt"))
                                            && description.lines().count() > 1
                                        {
                                            data.description = description;
                                        }

                                        let id = if is_workshop {
                                            ModId::Workshop(data.published_file_id)
                                        } else {
                                            ModId::Local(entry.clone())
                                        };
                                        let compat = self
                                            .library
                                            .compatibility
                                            .entry(id.clone())
                                            .or_default();
                                        compat.extend_with(xcom_mod::scan_compatibility(&entry));
                                        if is_workshop {
                                            self.downloaded.insert(data.published_file_id, entry);
                                        } else {
                                            self.local.insert(entry);
                                        }
                                        self.file_cache.update_mod_metadata(id.clone(), data);
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
                            eprintln!("Error scanning files in {}: {err:?}", directory.display());
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

        // I don't know if there's a cleaner way to do this
        let iter: Box<dyn Iterator<Item = (ModId, &PathBuf)>> = if is_workshop {
            Box::new(
                self.downloaded
                    .iter()
                    .map(|(id, path)| (ModId::Workshop(*id), path)),
            )
        } else {
            Box::new(
                self.local
                    .iter()
                    .filter(|path| path.starts_with(directory))
                    .map(|path| (ModId::Local(path.to_path_buf()), path)),
            )
        };

        for (id, path) in iter.into_iter() {
            if let Some(details) = self.file_cache.get_details(&id) {
                // Raw read
                let mut metadata = metadata::read_in(path).unwrap_or_default();
                metadata.tags.extend(
                    details
                        .tags()
                        .iter()
                        .map(|tag| metadata::Tag::from(tag.tag.as_ref())),
                );
                if let Err(err) = self.file_cache.update_metadata(path, metadata) {
                    eprintln!("Error updating metadata for {id}: {err:?}");
                }
            }

            if let Some(item) = self.library.items.get_mut(&id) {
                item.path = path.clone();
            } else if let Some(details) = self.file_cache.get_details(&id) {
                self.library.items.insert(
                    id.clone(),
                    library::LibraryItem {
                        id: id.clone(),
                        title: details.title().to_owned(),
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
            .update_missing_dependencies(self.file_cache.clone());
        for profile in self.profiles.values_mut() {
            profile.update_compatibility_issues(self.file_cache.clone(), &self.library);
        }
    }

    fn item_downloaded(&self, id: u32) -> bool {
        if self.is_downloading(id) {
            return false;
        }

        // Heuristically check if download was incomplete,
        // within 10% of expected size in case Steam is not reporting
        // sizes correctly
        if let Some(ModDetails::Workshop(data)) = self.file_cache.get_details(ModId::Workshop(id))
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

    fn cache_item_image<S: AsRef<str>>(&self, url: S) -> Task<Message> {
        if url.as_ref().is_empty() {
            return Task::none();
        }

        if !self.images.contains_key(url.as_ref()) {
            let url = url.as_ref().to_string();
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

    fn trim_snapshots(&self) {
        for (profile_name, profile) in self.profiles.iter() {
            // TODO: Implement incremental snapshot rebasing, *for now* size is unbounded for incremental snapshots
            if profile.settings.automatic_snapshot_strategy
                == library::snapshot_strategy::INCREMENTAL
            {
                continue;
            }

            if profile.settings.automatic_snapshot_number_limit == 0
                && profile.settings.automatic_snapshot_size_limit == 0
            {
                continue;
            }

            let profile_name = profile_name.to_owned();
            let number_limit = profile
                .settings
                .automatic_snapshot_number_limit
                .apply(|n| if n == 0 { usize::MAX } else { n as usize });
            let size_limit = profile
                .settings
                .automatic_snapshot_size_limit
                .apply(|n| if n == 0 { u64::MAX } else { n as u64 });
            std::thread::spawn(move || {
                let result: std::io::Result<()> = try {
                    let mut paths = Vec::with_capacity(number_limit + 1);
                    let mut sizes = Vec::with_capacity(paths.capacity());
                    for entry in std::fs::read_dir(snapshot::AUTOMATIC_SNAPSHOTS_DIR.as_path())? {
                        let entry = entry?;
                        let path = entry.path();
                        let Some(name) = path
                            .file_name()
                            .expect("path should not end with ..")
                            .to_str()
                        else {
                            // Any automatic snapshots created specifically by LXCOMM should be valid UTF-8
                            continue;
                        };
                        let Some((prefix, suffix)) = name.split_once(&profile_name) else {
                            continue;
                        };
                        if prefix != "LXCOMM_AUTO_BASIC_"
                            || chrono::NaiveDateTime::parse_from_str(
                                suffix,
                                "_%Y.%m.%d_%Hh%Mm%Ss.zip",
                            )
                            .is_err()
                        {
                            continue;
                        }
                        let metadata = entry.metadata()?;
                        paths.push(path);
                        sizes.push(metadata.len());
                    }
                    let mut current_size = sizes.iter().sum::<u64>();
                    let mut indices = Vec::from_iter(0..paths.len());
                    indices.reverse();
                    while (current_size > size_limit || indices.len() > number_limit)
                        && let Some(i) = indices.pop()
                    {
                        std::fs::remove_file(&paths[i])?;
                        current_size -= sizes[i];
                    }
                };
                if let Err(err) = result {
                    eprintln!("Error trimming snapshots: {err:?}");
                }
            });
        }
    }

    fn trim_image_cache(&self) {
        // TODO: Make limit customizable
        const SIZE_LIMIT: u64 = files::GIGA;
        std::thread::spawn(|| -> std::io::Result<()> {
            struct ImageInfo {
                path: PathBuf,
                size: u64,
                accessed: std::time::SystemTime,
            }
            let mut images = Vec::new();
            let mut current_size = 0;
            for entry in std::fs::read_dir(web::IMAGE_DIR.as_path())? {
                let entry = entry?;
                let metadata = entry.metadata()?;
                if !metadata.is_file() {
                    continue;
                }
                current_size += metadata.len();
                images.push(ImageInfo {
                    path: entry.path(),
                    size: metadata.len(),
                    accessed: metadata.accessed()?,
                });
            }
            if current_size < SIZE_LIMIT {
                return Ok(());
            }

            images.sort_by(|a, b| a.accessed.cmp(&b.accessed));

            for ImageInfo { path, size, .. } in images {
                if current_size < SIZE_LIMIT {
                    break;
                }
                std::fs::remove_file(path)?;
                current_size -= size;
            }

            Ok(())
        });
    }
}

#[cfg(feature = "dbus")]
struct ConnectionRecipe(zbus::blocking::Connection);
#[cfg(feature = "dbus")]
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
        Box::pin(iced::stream::channel(
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
#[cfg(feature = "dbus")]
struct AppSessionInterface {
    sender: Sender<Message>,
}

#[cfg(feature = "dbus")]
#[zbus::interface(name = "io.github.phantomshift.lxcomm")]
impl AppSessionInterface {
    async fn focus_window(&mut self) {
        if let Err(err) = self.sender.send(Message::GainFocus).await {
            eprintln!("Error sending GainFocus message: {err:?}");
        }
    }
}

pub fn main() -> eyre::Result<()> {
    // Potential TODO - Find a more graceful way of solving this cross-platform
    #[cfg(not(feature = "dbus"))]
    #[allow(unused_variables)]
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
        let icon =
            iced::window::icon::from_file_data(include_bytes!("../assets/lxcomm_64x64.png"), None)?;

        #[cfg(feature = "dbus")]
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
        #[cfg(feature = "dbus")]
        let once_boot = RefCell::new(Some(App::boot(connection)?));
        #[cfg(not(feature = "dbus"))]
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
        #[cfg(target_os = "linux")]
        platform_specific: iced::window::settings::PlatformSpecific {
            #[cfg(feature = "flatpak")]
            application_id: FLATPAK_ID.to_string(),
            #[cfg(not(feature = "flatpak"))]
            application_id: "lxcomm".to_string(),
            ..Default::default()
        },
        icon: Some(icon),
        ..Default::default()
    };

    let application = iced::application(boot, App::update, App::view)
        .window(window_settings)
        .title("Linux XCOM2 Mod Manager")
        .theme(App::theme)
        .subscription(App::subscription)
        .exit_on_close_request(false)
        .font(iced_aw::ICED_AW_FONT_BYTES);

    // TODO - Figure out why icon fonts don't render properly on Windows.
    #[cfg(not(target_os = "windows"))]
    let application = application.font(iced_fonts::FONTAWESOME_FONT_BYTES);

    application.run()?;
    Ok(())
}
