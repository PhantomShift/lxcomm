use std::{
    collections::{BTreeSet, HashMap},
    fmt::Display,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use crate::{
    CACHE_DIR, Message, XCOM_APPID,
    collections::{Collection, CollectionSource, ImageSource},
    extensions::{SplitAtBoundary, URLArrayListable},
    files::{self, ModDetails},
    xcom_mod::ModId,
};
use eyre::Result;
use iced::{
    Length::Fill,
    futures::{SinkExt, Stream, StreamExt},
    widget::{Container, container, image},
};
use serde::{Deserialize, Serialize};
use steam_rs::{
    Steam,
    published_file_service::{
        self,
        get_details::FileDetailResult,
        query_files::{PublishedFileQueryType, PublishedFiles},
    },
};
use strum::VariantArray;
use tokio::io::AsyncWriteExt;

// Cache valid for 1 day
const DEFAULT_CACHE_TIME: u32 = 86400;
const PROFILE_SUMMARY_CACHE_TIME: u32 = 86400 * 7;

pub static IMAGE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = CACHE_DIR.join("images");
    std::fs::create_dir_all(&dir).expect("cache directory shouild be writable");
    dir
});

static PROFILE_SUMMARY_CACHE_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| CACHE_DIR.join("profile_summaries"));
static PROFILE_SUMMARY_CACHE: LazyLock<
    moka::sync::Cache<u64, Arc<steam_rs::steam_user::get_player_summaries::Player>>,
> = LazyLock::new(|| moka::sync::Cache::new(128));

pub fn image_path<S: AsRef<str>>(s: S) -> PathBuf {
    IMAGE_DIR
        .join(blake3::hash(s.as_ref().as_bytes()).to_string())
        .with_extension("jpg")
}

pub async fn load_image<S: AsRef<str>>(url: S) -> Result<iced::widget::image::Handle> {
    let path = image_path(url.as_ref());

    match path.try_exists() {
        Ok(true) => {
            let bytes = std::fs::read(path)?;
            return Ok(image::Handle::from_bytes(bytes));
        }
        Ok(false) => (),
        Err(err) => eprintln!(
            "Error attempting to load cached image at path {} ({}): {err}",
            path.display(),
            url.as_ref()
        ),
    };

    let response = reqwest::get(url.as_ref()).await?;
    let bytes = response.bytes().await?;
    let handle = image::Handle::from_bytes(bytes.clone());
    std::fs::write(&path, bytes)?;

    Ok(handle)
}

pub fn image_maybe<'a, S: AsRef<str>>(
    state: &'a HashMap<String, image::Handle>,
    id: S,
) -> Container<'a, Message> {
    if let Some(handle) = state.get(id.as_ref()) {
        container(image(handle))
    } else {
        container(
            iced_aw::Spinner::new()
                .height(Fill)
                .height(Fill)
                .circle_radius(4.0),
        )
        .center(Fill)
    }
}

pub fn image_from_source<'a>(
    state: &'a HashMap<String, image::Handle>,
    source: ImageSource,
) -> Container<'a, Message> {
    match source {
        ImageSource::Path(path) => container(image(path)),
        ImageSource::Web(url) if !url.is_empty() => image_maybe(state, url),
        ImageSource::Web(_) => container("NO IMAGE AVAILABLE").center(Fill),
    }
}

#[derive(Debug, Clone)]
pub enum ResolverMessage {
    Setup(iced::futures::channel::mpsc::Sender<Message>),
    UpdateClient(Option<String>),
    RequestResolve(Vec<u32>),
    Resolved(Arc<Vec<steam_rs::published_file_service::query_files::File>>),
}

impl From<ResolverMessage> for Message {
    fn from(value: ResolverMessage) -> Self {
        Message::BackgroundResolverMessage(value)
    }
}

pub fn setup_background_resolver() -> impl Stream<Item = Message> {
    iced::stream::channel(256, async |mut output| {
        let (sender, mut receiver) = iced::futures::channel::mpsc::channel(256);
        if let Err(err) = output
            .send(Message::BackgroundResolverMessage(ResolverMessage::Setup(
                sender,
            )))
            .await
        {
            eprintln!(
                "Error sending sender, automatic resolution of unknown files will be unavailable"
            );
            eprintln!("Error: {err:?}");
            return;
        }

        let mut client = None;
        let mut unresolved = Vec::new();
        loop {
            if let Some(Message::BackgroundResolverMessage(message)) = receiver.next().await {
                match message {
                    ResolverMessage::UpdateClient(opt_client) => {
                        client = opt_client.map(|s| steam_rs::Steam::new(&s))
                    }
                    ResolverMessage::RequestResolve(unknown) => unresolved.extend(unknown),
                    ResolverMessage::Resolved(_) | ResolverMessage::Setup(_) => (),
                }
            }

            while !unresolved.is_empty()
                && let Some(current_client) = &client
            {
                let range = ..128.min(unresolved.len());
                match get_mod_details(current_client.clone(), &unresolved[range]).await {
                    Ok(files) => {
                        if let Err(err) = output
                            .send(Message::BackgroundResolverMessage(
                                ResolverMessage::Resolved(Arc::new(files)),
                            ))
                            .await
                        {
                            eprintln!("Error sending grabbed file details: {err:?}");
                        } else {
                            unresolved.drain(range);
                        }
                    }
                    Err(err) => {
                        eprintln!("Error resolving files: {err:?}");
                        // TODO: Properly grab error code
                        if err.to_string().contains("401 Unauthorized") {
                            client = None;
                            eprintln!(
                                "The API key the background resolver was sent does not appear to be valid."
                            );
                        }
                    }
                }
                if !unresolved.is_empty() {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
    })
}

macro_rules! async_retry {
    ($attempt:expr, $tries:expr, $debounce:expr $(,)*) => {{
        let max: usize = $tries;
        let mut attempts = 0;
        loop {
            match $attempt.await {
                Ok(result) => break Ok(result),
                Err(err) if attempts >= max => break Err(err),
                _ => {
                    tokio::time::sleep($debounce).await;
                    attempts += 1;
                }
            }
        }
    }};

    ($attempt:expr, $tries:expr $(,)*) => {
        async_retry!($attempt, $tries, std::time::Duration::ZERO)
    };
}

pub async fn resolve_all_dependencies(
    id: u32,
    client: Steam,
    mut cache: files::Cache,
) -> Result<Arc<BTreeSet<u32>>> {
    let mut dependencies = BTreeSet::new();
    let mut unresolved = vec![id];
    while !unresolved.is_empty() {
        let mut had_cached = true;
        while had_cached {
            had_cached = false;
            let mut to_extend = Vec::with_capacity(unresolved.len());

            unresolved.retain(|id| {
                if let Some(ModDetails::Workshop(details)) = cache.get_details(ModId::Workshop(*id))
                {
                    had_cached = true;
                    dependencies.insert(*id);
                    to_extend.extend(
                        details
                            .children
                            .iter()
                            .filter_map(|child| child.published_file_id.parse::<u32>().ok()),
                    );
                    false
                } else {
                    true
                }
            });

            unresolved.extend(to_extend);
        }

        if unresolved.is_empty() {
            break;
        }

        unresolved.sort();
        unresolved.dedup();

        let resolved = async_retry!(
            get_mod_details(client.clone(), &unresolved),
            5,
            std::time::Duration::from_millis(250)
        )?;

        unresolved.clear();

        for file in resolved {
            unresolved.extend(
                file.children
                    .iter()
                    .filter_map(|child| child.published_file_id.parse::<u32>().ok()),
            );
            let id = file.published_file_id.parse::<u32>()?;
            dependencies.insert(id);
            if let Err(err) = cache.insert_details(id, &Arc::new(file)) {
                eprintln!("Error writing dependency details to cache: {err:?}");
            }
        }
    }

    Ok(Arc::new(dependencies))
}

/// Prefer async if it is not known that all
/// unknown dependencies have been resolved.
pub fn resolve_all_dependencies_blocking(
    id: u32,
    client: Steam,
    cache: files::Cache,
) -> Result<Arc<BTreeSet<u32>>> {
    iced::futures::executor::block_on(resolve_all_dependencies(id, client, cache))
}

/// XCOM2 Workshop item tags. This is assumed to be a static list.
#[derive(
    Debug,
    Clone,
    Copy,
    strum::Display,
    Hash,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    VariantArray,
    strum::AsRefStr,
    Serialize,
    Deserialize,
)]
pub enum XCOM2WorkshopTag {
    #[strum(to_string = "War of the Chosen")]
    WarOfTheChosen,
    #[strum(to_string = "Soldier Class")]
    SoldierClass,
    #[strum(to_string = "Soldier Customization")]
    SoldierCustomization,
    Facility,
    Voice,
    UI,
    Item,
    Weapon,
    Map,
    Alien,
    Gameplay,
}

#[derive(Debug, Clone, Copy, Default, Hash, PartialEq, Eq, PartialOrd, Ord, VariantArray)]
pub enum WorkshopTrendPeriod {
    Today,
    #[default]
    Week,
    ThreeMonths,
    SixMonths,
    OneYear,
    // TODO - Figure out how to use all time through Steam web API, if possible.
    // URL parameter simply accepts `-1` in the workshop site itself,
    // but this doesn't seem to work with the web API.
    // Numbers > 365 seem to be truncated down to 365.
    // AllTime,
}

impl Display for WorkshopTrendPeriod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = format!("{self:?}");
        let display = name.split_at_case().collect::<Vec<_>>().join(" ");
        f.write_str(&display)
    }
}

impl WorkshopTrendPeriod {
    fn as_days(&self) -> u32 {
        match self {
            WorkshopTrendPeriod::Today => 1,
            WorkshopTrendPeriod::Week => 7,
            WorkshopTrendPeriod::ThreeMonths => 30,
            WorkshopTrendPeriod::SixMonths => 180,
            WorkshopTrendPeriod::OneYear => 365,
            // WorkshopTrendPeriod::AllTime => -1,
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkshopSort {
    Trend(WorkshopTrendPeriod),
    MostRecent,
    LastUpdated,
    TotalUniqueSubscribers,
    TextSearch,
}

impl Default for WorkshopSort {
    fn default() -> Self {
        Self::Trend(WorkshopTrendPeriod::default())
    }
}

impl Display for WorkshopSort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Trend(_) => "Most Popular",
            Self::MostRecent => "Most Recent",
            Self::LastUpdated => "Last Updated",
            Self::TotalUniqueSubscribers => "Most Subscribed",
            Self::TextSearch => "Relevance",
        };

        f.write_str(s)
    }
}

impl WorkshopSort {
    fn as_query_type(&self) -> PublishedFileQueryType {
        match self {
            Self::Trend(_) => PublishedFileQueryType::RankedByTrend,
            Self::MostRecent => PublishedFileQueryType::RankedByPublicationDate,
            Self::LastUpdated => PublishedFileQueryType::RankedByLastUpdatedDate,
            Self::TotalUniqueSubscribers => {
                PublishedFileQueryType::RankedByTotalUniqueSubscriptions
            }
            Self::TextSearch => PublishedFileQueryType::RankedByTextSearch,
        }
    }

    pub fn all_with_period(period: WorkshopTrendPeriod) -> Vec<Self> {
        vec![
            Self::Trend(period),
            Self::MostRecent,
            Self::LastUpdated,
            Self::TotalUniqueSubscribers,
            Self::TextSearch,
        ]
    }

    pub fn period_or_default(&self) -> WorkshopTrendPeriod {
        if let WorkshopSort::Trend(period) = self {
            period.to_owned()
        } else {
            Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkshopQuery {
    pub query: String,
    pub sort: WorkshopSort,
    pub tags: BTreeSet<XCOM2WorkshopTag>,
}

impl std::hash::Hash for WorkshopQuery {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.query.to_lowercase().hash(state);
        self.sort.hash(state);
        self.tags.hash(state);
    }
}

impl WorkshopQuery {
    pub fn new<S: Into<String>>(query: S) -> Self {
        Self {
            query: query
                .into()
                .replace(|ch: char| ch.is_ascii_whitespace(), "+"),
            ..Default::default()
        }
    }

    pub fn with_sort(self, sort: WorkshopSort) -> Self {
        Self { sort, ..self }
    }

    pub fn with_tags(self, tags: impl IntoIterator<Item = XCOM2WorkshopTag>) -> Self {
        Self {
            tags: BTreeSet::from_iter(tags),
            ..self
        }
    }

    pub fn as_hashed(&self) -> u64 {
        let mut hasher = std::hash::DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

// TODO - Introduce option for size limit of on-disk cache
static QUERY_CACHE: LazyLock<moka::future::Cache<String, PublishedFiles>> =
    LazyLock::new(|| moka::future::Cache::new(64));
pub const QUERY_PAGE_SIZE: u32 = 30;
pub async fn query_mods(
    client: Steam,
    page: u32,
    query: WorkshopQuery,
    cache_lifetime: u32,
) -> Result<(PublishedFiles, bool)> {
    let cache_key = format!("{}_page_{page:05}", query.as_hashed());

    if let Some(file) = QUERY_CACHE.get(&cache_key).await {
        return Ok((file, true));
    }

    let cached = crate::CACHED_QUERIES.join(format!("{cache_key}.json"));
    if let Ok(meta) = tokio::fs::metadata(&cached).await
        && let Ok(created) = meta.created()
    {
        if std::time::SystemTime::now()
            .duration_since(created)
            .expect("creation time should always be lower than now")
            .as_secs()
            <= cache_lifetime as u64
        {
            if let Ok(cached_file) = std::fs::File::open(&cached)
                && let Ok(cached_files) = serde_json::from_reader::<_, PublishedFiles>(cached_file)
            {
                QUERY_CACHE.insert(cache_key, cached_files.clone()).await;
                return Ok((cached_files, true));
            }
        } else {
            tokio::fs::remove_file(&cached).await?;
        }
    }

    let query = client
        .query_files(
            query.sort.as_query_type(),
            page,
            "",
            Some(QUERY_PAGE_SIZE),
            0,
            XCOM_APPID,
            &format!("&{}", query.tags.clone().into_url_array("requiredtags")),
            "",
            Some(!query.tags.is_empty()),
            "",
            "",
            &query.query,
            steam_rs::published_file_service::query_files::PublishedFileInfoMatchingFileType::Items,
            0,
            match &query.sort {
                WorkshopSort::Trend(period) => period.as_days(),
                _ => 0,
            },
            false,
            Some(DEFAULT_CACHE_TIME),
            None,
            "",
            false,
            false,
            true,
            true,
            true,
            true,
            true,
            false,
            false,
            Some(true),
            1,
        )
        .await?;

    if let Ok(cached) = std::fs::File::create(cached) {
        serde_json::to_writer(cached, &query)?;
    }

    QUERY_CACHE.insert(cache_key, query.clone()).await;

    // Pre-emptively resolve author information
    // Yes it is yucky that this is a side-effect.
    let unknown_authors = Vec::from_iter(query.published_file_details.iter().filter_map(|file| {
        PROFILE_SUMMARY_CACHE
            .optionally_get_with(file.creator.0, || {
                get_user_summary_disk_cached(file.creator.0).map(Arc::new)
            })
            .is_none()
            .then_some(file.creator.0.into())
    }));
    if !unknown_authors.is_empty()
        && let Ok(list) = client.get_player_summaries(unknown_authors).await
    {
        for info in list {
            let user_id = info.steam_id.0;
            if let Ok(mut file) =
                tokio::fs::File::create(PROFILE_SUMMARY_CACHE_DIR.join(format!("{user_id}.json")))
                    .await
                && let Ok(to_cache) = serde_json::to_vec(&info)
            {
                let _ = file.write_all(&to_cache).await;
            }
            PROFILE_SUMMARY_CACHE.insert(user_id, Arc::new(info));
        }
    }

    Ok((query, false))
}

// Only doing per-session caching for collections since
// I expect that it's less likely to be extensively browsed
static COLLECTION_QUERY_CACHE: LazyLock<
    moka::sync::Cache<(u32, WorkshopQuery), CollectionQueryResponse>,
> = LazyLock::new(|| {
    moka::sync::Cache::builder()
        .time_to_idle(std::time::Duration::from_secs(3600))
        .max_capacity(128)
        .build()
});

#[derive(Debug, Clone)]
pub struct CollectionQueryResponse {
    pub total: u64,
    pub collections: Arc<[Collection]>,
}

pub async fn query_collections(
    client: Steam,
    page: u32,
    query: WorkshopQuery,
) -> Result<CollectionQueryResponse> {
    if let Some(collections) = COLLECTION_QUERY_CACHE.get(&(page, query.clone())) {
        return Ok(collections);
    }

    let response = client
        .query_files(
            query.sort.as_query_type(),
            page,
            "",
            Some(QUERY_PAGE_SIZE),
            0,
            XCOM_APPID,
            &format!("&{}", query.tags.iter().into_url_array("requiredtags")),
            "",
            Some(!query.tags.is_empty()),
            "",
            "",
            &query.query,
            steam_rs::published_file_service::query_files::PublishedFileInfoMatchingFileType::Collections,
            0,
            match &query.sort {
                WorkshopSort::Trend(period) => period.as_days(),
                _ => 0,
            },
            false,
            Some(DEFAULT_CACHE_TIME),
            None,
            "",
            false,
            false,
            true,
            true,
            true,
            true,
            true,
            false,
            false,
            Some(true),
            1,
        )
        .await?;

    let total = response.total;

    let collections = response
        .published_file_details
        .into_iter()
        .filter_map(|file| {
            let id = file.published_file_id.parse::<u32>().ok()?;
            let published_file_service::query_files::File {
                title,
                children,
                preview_url,
                file_description,
                previews,
                ..
            } = file;
            let banner_url = previews
                .iter()
                .find_map(|prev| prev.preview_type.eq(&0).then(|| prev.url.clone()))
                .flatten();
            Some(Collection {
                source: CollectionSource::Workshop(id),
                title,
                items: children
                    .iter()
                    .flat_map(|child| child.published_file_id.parse::<u32>())
                    .collect(),
                image: ImageSource::Web(preview_url),
                banner: banner_url.map(ImageSource::Web),
                description: file_description.unwrap_or_default(),
            })
        })
        .collect::<Arc<[_]>>();

    let response = CollectionQueryResponse { total, collections };

    COLLECTION_QUERY_CACHE.insert((page, query.clone()), response.clone());

    Ok(response)
}

/// note - no cache check is done as mods are assumed to be
/// unknown if this is called, otherwise it is cached somewhere on the filesystem.
pub async fn get_mod_details<U: Copy + Into<u64>>(
    client: Steam,
    ids: impl AsRef<[U]>,
) -> Result<Vec<steam_rs::published_file_service::query_files::File>> {
    let ids = ids.as_ref().iter().map(|u| (*u).into()).fold(
        Vec::with_capacity(ids.as_ref().len()),
        |mut v, id| {
            v.push(id);
            v
        },
    );

    let details = client
        .get_details(
            ids,
            true,
            true,
            true,
            true,
            true,
            false,
            false,
            true,
            None,
            1,
            XCOM_APPID,
            false,
            None,
            Some(true),
            false,
        )
        .await?;

    Ok(details
        .published_file_details
        .into_iter()
        .filter_map(|result| match result {
            FileDetailResult::Valid(file) => Some(file),
            FileDetailResult::Invalid(_) => None,
        })
        .collect())
}

/// Returns the the list of mods in the given list that were outdated, with the time it was updated.
pub async fn check_mods_outdated(
    client: Steam,
    items: Vec<(u64, u64)>,
) -> Result<HashMap<u32, u64>> {
    let ids = items.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let mut outdated = HashMap::with_capacity(ids.len());

    // Potential TODO - allow partial failures
    const PER_BATCH_MAX: usize = 128;
    let mut index = 0;
    while index < ids.len() {
        let range = index..(index + PER_BATCH_MAX).min(ids.len());
        index = range.end;
        let details = client
            .get_details(
                &ids[range],
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                false,
                None,
                1,
                XCOM_APPID,
                false,
                None,
                None,
                false,
            )
            .await?;

        for details in details.published_file_details {
            if let FileDetailResult::Valid(file) = details
                && let Ok(file_id) = file.published_file_id.parse::<u64>()
                && let Some((_, last_updated)) = items.iter().find(|(id, _)| *id == file_id)
                && file.time_updated as u64 > *last_updated
            {
                outdated.insert(file_id as u32, file.time_updated as u64);
            }
        }
    }

    Ok(outdated)
}

fn get_user_summary_disk_cached(
    user_id: u64,
) -> Option<steam_rs::steam_user::get_player_summaries::Player> {
    if !PROFILE_SUMMARY_CACHE_DIR.exists() {
        std::fs::create_dir_all(PROFILE_SUMMARY_CACHE_DIR.as_path()).ok()?;
    }
    let cached_path = PROFILE_SUMMARY_CACHE_DIR.join(format!("{user_id}.json"));
    if let Ok(true) = cached_path.try_exists()
        && let Ok(metadata) = cached_path.metadata()
        && let Ok(created) = metadata.created()
        && std::time::SystemTime::now()
            .duration_since(created)
            .is_ok_and(|d| d.as_secs() <= PROFILE_SUMMARY_CACHE_TIME as u64)
        && let Ok(bytes) = std::fs::read(&cached_path)
        && let Ok(cached) = serde_json::from_slice(&bytes)
    {
        Some(cached)
    } else {
        None
    }
}

pub fn get_user_display_name(client: Steam, user_id: u64) -> String {
    PROFILE_SUMMARY_CACHE
        .optionally_get_with(user_id, move || {
            if let Some(cached) = get_user_summary_disk_cached(user_id) {
                return Some(Arc::new(cached));
            }
            let cached_path = PROFILE_SUMMARY_CACHE_DIR.join(format!("{user_id}.json"));

            // Wishing there was an easier way to ensure this doesn't panic in case this gets called in an async context
            let resolve = async move { client.get_player_summaries(vec![user_id.into()]).await };
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.block_on(resolve).ok()
            } else {
                tokio::runtime::Runtime::new()
                    .map(|rt| rt.block_on(resolve).ok())
                    .ok()
                    .flatten()
            }
            .and_then(|v| v.into_iter().next())
            .inspect(|info| {
                if let Ok(file) = std::fs::File::create(cached_path) {
                    let _ = serde_json::to_writer(file, info);
                }
            })
            .map(Arc::new)
        })
        .map(|player| player.persona_name.clone())
        .unwrap_or("UNKNOWN".to_string())
}

pub fn handle_url(url: String) -> Message {
    let Ok(url) = reqwest::Url::parse(&url) else {
        return Message::None;
    };

    if url.domain() == Some("steamcommunity.com")
        && url
            .path_segments()
            .is_some_and(|mut split| split.nth_back(1) == Some("filedetails"))
        && let Some(id) = url
            .query_pairs()
            .find_map(|(name, value)| name.eq("id").then(|| value.parse::<u32>().ok()))
            .flatten()
    {
        Message::SetViewingItem(ModId::Workshop(id))
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

/// Should only be used for trusted links (e.g. Steam)
pub fn open_browser(url: String) -> Message {
    std::thread::spawn(move || {
        let _ = opener::open_browser(url);
    });
    Message::None
}
