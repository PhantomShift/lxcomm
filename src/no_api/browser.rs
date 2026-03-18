use std::{
    collections::HashMap,
    ops::Not,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, LazyLock},
};

use apply::Apply;
use iced::Task;
use workshop_reader::{self, PageProvider};

use crate::{XCOM_APPID, reset_scroll, web};

static CLIENT_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
static WEBCACHE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let path = crate::CACHE_DIR.join("noapi_webcache");
    if !path.exists() {
        std::fs::create_dir_all(&path).expect("cache directory should be writable");
    }
    path
});

pub trait CacheProvider: Clone {
    fn get_item(&self, id: u64) -> Option<Arc<workshop_reader::WorkshopFile>>;
    fn get_search(&self, query: &str) -> Option<Arc<workshop_reader::QueryResult>>;
    fn set_item(&self, id: u64, val: Arc<workshop_reader::WorkshopFile>);
    fn set_search(&self, query: &str, val: Arc<workshop_reader::QueryResult>);
}

#[derive(Debug, Clone)]
pub struct DefaultCacheProvider {
    items: moka::sync::Cache<u64, Arc<workshop_reader::WorkshopFile>>,
    searches: moka::sync::Cache<String, Arc<workshop_reader::QueryResult>>,
}

impl CacheProvider for DefaultCacheProvider {
    fn get_item(&self, id: u64) -> Option<Arc<workshop_reader::WorkshopFile>> {
        self.items.get(&id)
    }
    fn get_search(&self, query: &str) -> Option<Arc<workshop_reader::QueryResult>> {
        self.searches.get(query)
    }

    fn set_item(&self, id: u64, val: Arc<workshop_reader::WorkshopFile>) {
        self.items.insert(id, val);
    }
    fn set_search(&self, query: &str, val: Arc<workshop_reader::QueryResult>) {
        self.searches.insert(query.to_string(), val);
    }
}

#[derive(Debug, Clone)]
pub struct WorkshopClient<C: CacheProvider = DefaultCacheProvider> {
    cache: C,
    client: reqwest::Client,
    rate_limiter: Arc<governor::DefaultDirectRateLimiter>,
    webcache: super::webcache::WebCache,

    query: workshop_reader::QueryParams,
    page: u32,
    max_page: u32,
    lifetime: u32,
}

impl<C> workshop_reader::PageProvider for WorkshopClient<C>
where
    C: CacheProvider + Sync,
{
    type Error = reqwest::Error;

    async fn request_page(&self, url: String) -> Result<String, Self::Error> {
        let limit = chrono::Utc::now() - std::time::Duration::from_secs(self.lifetime as u64);
        if let Some(cached) = self.webcache.get_entry_after(&url, limit) {
            return Ok(cached.page);
        }

        self.rate_limiter.until_ready().await;
        if let Some(cached) = self.webcache.get_entry(&url) {
            return Ok(cached.page);
        }

        let response = self.client.get(&url).send().await?;
        let body = response.text().await?;

        if let Err(err) = self.webcache.cache_page(&url, &body) {
            eprintln!("Failed to write page to disk cache: {err:?}");
        }

        Ok(body)
    }

    async fn request_item_details(
        &self,
        published_file_id: u64,
    ) -> Result<Arc<workshop_reader::WorkshopFile>, workshop_reader::error::Error> {
        if let Some(details) = self.cache.get_item(published_file_id) {
            Ok(details)
        } else {
            let page = self
                .request_page_wrapped(Self::build_item_url(published_file_id))
                .await?;
            Self::parse_item(&page)
                .map(|f| workshop_reader::WorkshopFile {
                    published_file_id,
                    ..f
                })
                .map(Arc::new)
                .inspect(|arc| {
                    self.cache.set_item(published_file_id, arc.clone());
                })
        }
    }

    async fn query_items(
        &self,
        app_id: u64,
        page: u32,
        params: workshop_reader::QueryParams,
    ) -> Result<Arc<workshop_reader::QueryResult>, workshop_reader::error::Error> {
        let query = Self::build_browse_url(app_id, page, params);
        if let Some(result) = self.cache.get_search(&query) {
            Ok(result)
        } else {
            let page = self.request_page_wrapped(query.clone()).await?;
            Self::parse_browse(&page).map(Arc::new).inspect(|arc| {
                self.cache.set_search(&query, arc.clone());
            })
        }
    }
}

pub struct WorkshopItemsBrowser<C>
where
    C: CacheProvider + Sync,
{
    client: WorkshopClient<C>,

    edit_query: workshop_reader::QueryParams,
    edit_period: workshop_reader::QueryPeriod,
    scroll_id: iced::widget::Id,
    tags_toggled: bool,
    query_result: Option<Arc<workshop_reader::QueryResult>>,
}

#[derive(Debug, Clone)]
pub enum WorkshopClientMessage {
    Page(u32),
    SubmitQuery,
    EditQueryText(String),
    EditSort(web::WorkshopSort),
    EditPeriod(web::WorkshopTrendPeriod),
    ToggleTags(bool),
    EditTag(web::XCOM2WorkshopTag),

    ResolveQuery(Arc<workshop_reader::QueryResult>),
}

impl From<WorkshopClientMessage> for crate::Message {
    fn from(value: WorkshopClientMessage) -> Self {
        Self::WorkshopMessageNoAPI(value)
    }
}

// TODO: Use types directly
impl From<workshop_reader::QuerySort> for crate::web::WorkshopSort {
    fn from(value: workshop_reader::QuerySort) -> Self {
        use web::{WorkshopSort, WorkshopTrendPeriod};
        use workshop_reader::{QueryPeriod, QuerySort};
        match value {
            QuerySort::Trend(period) => match period {
                QueryPeriod::Today => WorkshopTrendPeriod::Today,
                QueryPeriod::Week => WorkshopTrendPeriod::Week,
                QueryPeriod::ThreeMonths => WorkshopTrendPeriod::ThreeMonths,
                QueryPeriod::SixMonths => WorkshopTrendPeriod::SixMonths,
                QueryPeriod::OneYear => WorkshopTrendPeriod::OneYear,
                QueryPeriod::AllTime => WorkshopTrendPeriod::AllTime,
            }
            .apply(WorkshopSort::Trend),
            QuerySort::LastUpdated => WorkshopSort::LastUpdated,
            QuerySort::MostRecent => WorkshopSort::MostRecent,
            QuerySort::TextSearch => WorkshopSort::TextSearch,
            QuerySort::TotalUniqueSubscribers => WorkshopSort::TotalUniqueSubscribers,
        }
    }
}

impl From<crate::web::WorkshopSort> for workshop_reader::QuerySort {
    fn from(value: crate::web::WorkshopSort) -> Self {
        use web::WorkshopSort;
        use workshop_reader::QuerySort;
        match value {
            WorkshopSort::Trend(period) => QuerySort::Trend(period.into()),
            WorkshopSort::LastUpdated => QuerySort::LastUpdated,
            WorkshopSort::MostRecent => QuerySort::MostRecent,
            WorkshopSort::TextSearch => QuerySort::TextSearch,
            WorkshopSort::TotalUniqueSubscribers => QuerySort::TotalUniqueSubscribers,
        }
    }
}

impl From<crate::web::WorkshopTrendPeriod> for workshop_reader::QueryPeriod {
    fn from(value: crate::web::WorkshopTrendPeriod) -> Self {
        use web::WorkshopTrendPeriod;
        use workshop_reader::QueryPeriod;
        match value {
            WorkshopTrendPeriod::Today => QueryPeriod::Today,
            WorkshopTrendPeriod::Week => QueryPeriod::Week,
            WorkshopTrendPeriod::ThreeMonths => QueryPeriod::ThreeMonths,
            WorkshopTrendPeriod::SixMonths => QueryPeriod::SixMonths,
            WorkshopTrendPeriod::OneYear => QueryPeriod::OneYear,
            WorkshopTrendPeriod::AllTime => QueryPeriod::AllTime,
        }
    }
}

impl<C> crate::browser::WorkshopBrowser for WorkshopItemsBrowser<C>
where
    C: CacheProvider + Sync,
{
    type Item = workshop_reader::QueryItem;
    type Message = crate::Message;

    fn get_max_page(&self) -> u32 {
        self.client.max_page
    }

    fn get_page(&self) -> u32 {
        self.client.page
    }

    fn get_query(&self) -> crate::web::WorkshopQuery {
        let params = &self.edit_query;
        crate::web::WorkshopQuery::new(params.search_text.clone())
            .with_sort(params.sort_method.into())
            .with_tags(
                params
                    .tags
                    .iter()
                    .filter_map(|tag| crate::web::XCOM2WorkshopTag::from_str(tag).ok()),
            )
    }

    fn get_scroll_id(&self) -> iced::widget::Id {
        self.scroll_id.clone()
    }

    fn get_tags_toggled(&self) -> bool {
        self.tags_toggled
    }

    fn on_page_change(&self, _state: &crate::App, new: u32) -> Self::Message {
        WorkshopClientMessage::Page(new).into()
    }

    fn on_query_period_edited(
        &self,
        _state: &crate::App,
        new: crate::web::WorkshopTrendPeriod,
    ) -> Self::Message {
        WorkshopClientMessage::EditPeriod(new).into()
    }

    fn on_query_sort_edited(&self, _state: &crate::App, new: web::WorkshopSort) -> Self::Message {
        WorkshopClientMessage::EditSort(new).into()
    }

    fn on_query_submitted(&self, _state: &crate::App) -> Self::Message {
        WorkshopClientMessage::SubmitQuery.into()
    }

    fn on_query_tag_edited(
        &self,
        _state: &crate::App,
        tag: web::XCOM2WorkshopTag,
    ) -> Self::Message {
        WorkshopClientMessage::EditTag(tag).into()
    }

    fn on_query_tags_toggled(&self, _state: &crate::App, toggled: bool) -> Self::Message {
        WorkshopClientMessage::ToggleTags(toggled).into()
    }

    fn on_query_text_edited(&self, _state: &crate::App, new: String) -> Self::Message {
        WorkshopClientMessage::EditQueryText(new).into()
    }

    fn render_preview_box<'a>(
        &'a self,
        state: &'a crate::App,
        item: &'a Self::Item,
    ) -> iced::Element<'a, crate::Message> {
        use iced::{
            Alignment::Center,
            Length::Fill,
            widget::{button, column, text},
        };

        let id = item.id;

        column![
            web::image_maybe(&state.images, &item.preview_url)
                .width(Fill)
                .height(300),
            text(&item.title),
            text(id),
            text(&item.author_name),
            text!("{} out of 5", item.stars),
            button(text("View").align_x(Center))
                .width(Fill)
                // compat, eventually change all IDs to u64
                .on_press(crate::Message::SetViewingItem((id as u32).into())),
            button(
                text(if state.item_downloaded(id as u32) {
                    "Update"
                } else {
                    "Download"
                })
                .align_x(Center)
            )
            .width(Fill)
            .on_press_maybe(
                state
                    .is_downloading(id as u32)
                    .not()
                    .then_some(crate::Message::SteamCMDDownloadRequested(id as u32))
            ),
            item.short_description
                .as_ref()
                .map(|desc| text(desc).shaping(text::Shaping::Advanced)),
        ]
        .into()
    }
}

impl WorkshopItemsBrowser<DefaultCacheProvider> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cache(&self) -> DefaultCacheProvider {
        self.client.cache.clone()
    }

    pub fn clear_cache(&self) {
        self.client.cache.items.invalidate_all();
        self.client.cache.items.invalidate_all();
        if let Err(err) = self.client.webcache.clear() {
            eprintln!("Error clearing web cache: {err:?}");
        }
    }

    pub fn set_lifetime(&mut self, lifetime: u32) {
        if lifetime > 0 {
            self.client.lifetime = lifetime;
        } else {
            self.client.lifetime = crate::web::DEFAULT_CACHE_TIME;
        }
        self.client
            .webcache
            .set_lifetime(self.client.lifetime as u64);
    }

    pub fn get_items(
        &self,
    ) -> impl Iterator<Item = &<Self as crate::browser::WorkshopBrowser>::Item> {
        self.query_result
            .as_ref()
            .map(|query| query.items.iter())
            .into_iter()
            .flatten()
    }

    pub fn update(
        &mut self,
        images: &HashMap<String, iced::widget::image::Handle>,
        message: WorkshopClientMessage,
    ) -> Task<crate::Message> {
        match message {
            WorkshopClientMessage::Page(page) => {
                let new = std::cmp::max(page, 1);
                if new == self.client.page {
                    return Task::none();
                }
                self.client.page = new;

                return self.update(images, WorkshopClientMessage::SubmitQuery);
            }
            WorkshopClientMessage::SubmitQuery => {
                if let workshop_reader::QuerySort::Trend(period) = &mut self.edit_query.sort_method
                {
                    *period = self.edit_period;
                }
                self.client.page = std::cmp::max(self.client.page, 1);
                self.client.query = self.edit_query.clone();

                let client = self.client.clone();
                let scroll_task = reset_scroll!(self.scroll_id.clone());
                return Task::done(crate::Message::SetBusy(true))
                    .chain(Task::future(async move {
                        match client
                            .query_items(XCOM_APPID as u64, client.page, client.query.clone())
                            .await
                        {
                            Ok(query) => WorkshopClientMessage::ResolveQuery(query).into(),
                            Err(err) => {
                                crate::Message::display_error("Page Load Failed", err.to_string())
                            }
                        }
                    }))
                    .chain(scroll_task)
                    .chain(Task::done(crate::Message::SetBusy(false)));
            }

            WorkshopClientMessage::EditQueryText(s) => self.edit_query.search_text = s,
            WorkshopClientMessage::EditSort(sort) => self.edit_query.sort_method = sort.into(),
            WorkshopClientMessage::EditPeriod(period) => self.edit_period = period.into(),
            WorkshopClientMessage::ToggleTags(toggled) => self.tags_toggled = toggled,
            WorkshopClientMessage::EditTag(tag) => {
                if self.edit_query.tags.contains(tag.as_ref()) {
                    self.edit_query.tags.insert(tag.to_string());
                } else {
                    self.edit_query.tags.remove(tag.as_ref());
                }
            }

            WorkshopClientMessage::ResolveQuery(resolved) => {
                let image_task = Task::batch(resolved.items.iter().filter_map(|item| {
                    let url = item.preview_url.clone();
                    images.contains_key(&item.preview_url).not().then(|| {
                        Task::future(async move {
                            match web::load_image(&url).await {
                                Ok(path) => {
                                    // Not sure if this is strictly necessary but for preventing overload
                                    tokio::time::sleep(std::time::Duration::from_millis(16)).await;
                                    crate::Message::ImageLoaded(url, path)
                                }
                                Err(err) => {
                                    eprintln!("Error attempting to load image: {err:?}");
                                    crate::Message::None
                                }
                            }
                        })
                    })
                }));

                let resolve_task = Task::batch(resolved.items.iter().filter_map(|item| {
                    let id = item.id;
                    self.client.cache.items.contains_key(&id).not().then(|| {
                        let client = self.client.clone();
                        Task::future(async move {
                            if let Err(err) = client.request_item_details(id).await {
                                eprintln!(
                                    "Error attempting to resolve file details for {id}: {err:?}"
                                );
                            }
                            crate::Message::None
                        })
                    })
                }));

                self.client.max_page = resolved.pages;
                self.query_result = Some(resolved);
                return Task::batch([image_task, resolve_task]);
            }
        }

        Task::none()
    }
}

impl Default for WorkshopItemsBrowser<DefaultCacheProvider> {
    fn default() -> Self {
        Self {
            client: WorkshopClient {
                cache: DefaultCacheProvider {
                    items: moka::sync::Cache::new(1024),
                    searches: moka::sync::Cache::new(64),
                },
                client: reqwest::ClientBuilder::new()
                    .user_agent(CLIENT_USER_AGENT)
                    .build()
                    .expect("failed to build reqwest client"),
                rate_limiter: Arc::new(governor::DefaultDirectRateLimiter::direct(unsafe {
                    governor::Quota::per_minute(std::num::NonZero::new_unchecked(8))
                        .allow_burst(std::num::NonZero::new_unchecked(4))
                })),
                webcache: super::webcache::WebCache::new(WEBCACHE_DIR.clone())
                    .expect("failed to initialize webcache"),
                query: Default::default(),
                page: 0,
                max_page: 0,
                lifetime: crate::web::DEFAULT_CACHE_TIME,
            },
            edit_query: Default::default(),
            edit_period: Default::default(),
            scroll_id: iced::widget::Id::unique(),
            tags_toggled: false,
            query_result: None,
        }
    }
}
