use std::{
    collections::{BTreeSet, HashMap},
    fmt::Display,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use crate::{
    CACHE_DIR, Message, XCOM_APPID,
    extensions::{SplitAtBoundary, URLArrayListable},
};
use eyre::Result;
use iced::{
    Length::Fill,
    futures::{SinkExt, Stream, StreamExt},
    widget::{Container, container, image},
};
use steam_rs::{
    Steam,
    published_file_service::{
        get_details::FileDetailResult,
        query_files::{PublishedFileQueryType, PublishedFiles},
    },
};
use strum::VariantArray;

// Cache valid for 1 day
const DEFAULT_CACHE_TIME: u32 = 86400;

pub static IMAGE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = CACHE_DIR.join("images");
    std::fs::create_dir_all(&dir).expect("cache directory shouild be writable");
    dir
});

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
    iced::stream::channel(100, async |mut output| {
        let (sender, mut receiver) = iced::futures::channel::mpsc::channel(100);
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

            if !unresolved.is_empty()
                && let Some(current_client) = &client
            {
                match get_mod_details(current_client.clone(), &unresolved).await {
                    Ok(files) => {
                        if let Err(err) = output
                            .send(Message::BackgroundResolverMessage(
                                ResolverMessage::Resolved(Arc::new(files)),
                            ))
                            .await
                        {
                            eprintln!("Error sending grabbed file details: {err:?}");
                        } else {
                            unresolved.clear();
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
            }
        }
    })
}

/// XCOM2 Workshop item tags. This is assumed to be a static list.
#[derive(
    Debug, Clone, Copy, strum::Display, Hash, PartialEq, Eq, PartialOrd, Ord, VariantArray,
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
}

#[derive(Debug, Clone, Default)]
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
    fn new<S: Into<String>>(query: S) -> Self {
        Self {
            query: query
                .into()
                .replace(|ch: char| ch.is_ascii_whitespace(), "+"),
            ..Default::default()
        }
    }

    fn with_sort(self, sort: WorkshopSort) -> Self {
        Self { sort, ..self }
    }

    fn with_tags(self, tags: impl IntoIterator<Item = XCOM2WorkshopTag>) -> Self {
        Self {
            tags: BTreeSet::from_iter(tags),
            ..self
        }
    }

    fn as_hashed(&self) -> u64 {
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
            XCOM_APPID,
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

    Ok((query, false))
}

/// note - no cache check is done as mods are assumed to be
/// unknown if this is called, otherwise it is cached somewhere on the filesystem.
pub async fn get_mod_details<U: Copy + Into<u64>>(
    client: Steam,
    ids: &[U],
) -> Result<Vec<steam_rs::published_file_service::query_files::File>> {
    let ids = ids.iter().map(|u| (*u).into()).collect();
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
