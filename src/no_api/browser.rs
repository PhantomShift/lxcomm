use std::{
    collections::{HashMap, HashSet},
    ops::Not,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, LazyLock, atomic::AtomicU64},
};

use apply::Apply;
use iced::{Task, futures::SinkExt};
use workshop_reader::{self, PageProvider};

use crate::{App, XCOM_APPID, reset_scroll, web};

static WEBCACHE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let path = crate::CACHE_DIR.join("noapi_webcache");
    if !path.exists() {
        std::fs::create_dir_all(&path).expect("cache directory should be writable");
    }
    path
});

pub const fn star_string(stars: u8) -> &'static str {
    match stars {
        0 => "☆☆☆☆☆",
        1 => "★☆☆☆☆",
        2 => "★★☆☆☆",
        3 => "★★★☆☆",
        4 => "★★★★☆",
        5.. => "★★★★★",
    }
}

pub trait CacheProvider: Clone {
    fn get_item(&self, id: u64) -> Option<Arc<workshop_reader::WorkshopFile>>;
    fn get_search(&self, query: &str) -> Option<Arc<workshop_reader::QueryItemResult>>;
    fn set_item(&self, id: u64, val: Arc<workshop_reader::WorkshopFile>);
    fn set_search(&self, query: &str, val: Arc<workshop_reader::QueryItemResult>);

    fn get_collection(&self, id: u64) -> Option<Arc<workshop_reader::WorkshopCollection>>;
    fn get_collection_search(
        &self,
        query: &str,
    ) -> Option<Arc<workshop_reader::QueryCollectionResult>>;
    fn set_collection(&self, id: u64, val: Arc<workshop_reader::WorkshopCollection>);
    fn set_collection_search(&self, query: &str, val: Arc<workshop_reader::QueryCollectionResult>);
}

#[derive(Debug, Clone)]
pub struct DefaultCacheProvider {
    items: moka::sync::Cache<u64, Arc<workshop_reader::WorkshopFile>>,
    searches: moka::sync::Cache<String, Arc<workshop_reader::QueryItemResult>>,

    // Unlike with the API, due to the potential of users being rate limited for activity,
    // we will also do disk caching for collections
    collections: moka::sync::Cache<u64, Arc<workshop_reader::WorkshopCollection>>,
    collection_searches: moka::sync::Cache<String, Arc<workshop_reader::QueryCollectionResult>>,
}

impl Default for DefaultCacheProvider {
    fn default() -> Self {
        Self {
            items: moka::sync::Cache::new(1024),
            searches: moka::sync::Cache::new(64),
            collections: moka::sync::Cache::new(128),
            collection_searches: moka::sync::Cache::new(32),
        }
    }
}

impl CacheProvider for DefaultCacheProvider {
    fn get_item(&self, id: u64) -> Option<Arc<workshop_reader::WorkshopFile>> {
        self.items.get(&id)
    }
    fn get_search(&self, query: &str) -> Option<Arc<workshop_reader::QueryItemResult>> {
        self.searches.get(query)
    }

    fn set_item(&self, id: u64, val: Arc<workshop_reader::WorkshopFile>) {
        self.items.insert(id, val);
    }
    fn set_search(&self, query: &str, val: Arc<workshop_reader::QueryItemResult>) {
        self.searches.insert(query.to_string(), val);
    }

    fn get_collection(&self, id: u64) -> Option<Arc<workshop_reader::WorkshopCollection>> {
        self.collections.get(&id)
    }
    fn get_collection_search(
        &self,
        query: &str,
    ) -> Option<Arc<workshop_reader::QueryCollectionResult>> {
        self.collection_searches.get(query)
    }

    fn set_collection(&self, id: u64, val: Arc<workshop_reader::WorkshopCollection>) {
        self.collections.insert(id, val);
    }
    fn set_collection_search(&self, query: &str, val: Arc<workshop_reader::QueryCollectionResult>) {
        self.collection_searches.insert(query.to_string(), val);
    }
}

#[derive(Debug, Clone)]
pub struct WorkshopClient<C: CacheProvider = DefaultCacheProvider> {
    cache: C,
    client: reqwest::Client,
    rate_limiter: Arc<governor::DefaultDirectRateLimiter>,
    webcache: super::webcache::WebCache,

    lifetime: Arc<AtomicU64>,
}

impl<C: CacheProvider + Default> WorkshopClient<C> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<C: CacheProvider + Default> Default for WorkshopClient<C> {
    fn default() -> Self {
        Self {
            cache: Default::default(),
            client: reqwest::ClientBuilder::new()
                .user_agent(web::CLIENT_USER_AGENT)
                .build()
                .expect("failed to build reqwest client"),
            rate_limiter: Arc::new(governor::DefaultDirectRateLimiter::direct(unsafe {
                governor::Quota::per_minute(std::num::NonZero::new_unchecked(8))
                    .allow_burst(std::num::NonZero::new_unchecked(4))
            })),
            webcache: super::webcache::WebCache::new(WEBCACHE_DIR.clone())
                .expect("failed to initialize webcache"),
            lifetime: Arc::new(AtomicU64::new(crate::web::DEFAULT_CACHE_TIME as u64)),
        }
    }
}

impl<C> workshop_reader::PageProvider for WorkshopClient<C>
where
    C: CacheProvider + Sync,
{
    type Error = reqwest::Error;

    async fn request_page<U: reqwest::IntoUrl + Send>(
        &self,
        url: U,
    ) -> Result<String, Self::Error> {
        let url = url.into_url()?;
        let limit = chrono::Utc::now()
            - std::time::Duration::from_secs(
                self.lifetime.load(std::sync::atomic::Ordering::Relaxed),
            );
        if let Some(cached) = self.webcache.get_entry_after(url.as_str(), limit) {
            return Ok(cached.page);
        }

        self.rate_limiter.until_ready().await;
        if let Some(cached) = self.webcache.get_entry(url.as_str()) {
            return Ok(cached.page);
        }

        let response = self.client.get(url.clone()).send().await?;
        let body = response.text().await?;

        if let Err(err) = self.webcache.cache_page(url.as_str(), &body) {
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
    ) -> Result<Arc<workshop_reader::QueryItemResult>, workshop_reader::error::Error> {
        let query = Self::build_browse_url(app_id, page, params);
        if let Some(result) = self.cache.get_search(query.as_str()) {
            Ok(result)
        } else {
            let page = self.request_page_wrapped(query.clone()).await?;
            Self::parse_browse(&page).map(Arc::new).inspect(|arc| {
                self.cache.set_search(query.as_str(), arc.clone());
            })
        }
    }

    async fn request_collection_details(
        &self,
        id: u64,
    ) -> Result<Arc<workshop_reader::WorkshopCollection>, workshop_reader::error::Error> {
        if let Some(details) = self.cache.get_collection(id) {
            Ok(details)
        } else {
            let page = self.request_page_wrapped(Self::build_item_url(id)).await?;
            Self::parse_collection(&page)
                .map(|c| workshop_reader::WorkshopCollection { id, ..c })
                .map(Arc::new)
                .inspect(|arc| {
                    self.cache.set_collection(id, arc.clone());
                })
        }
    }

    async fn query_collections(
        &self,
        app_id: u64,
        page: u32,
        params: workshop_reader::QueryParams,
    ) -> Result<Arc<workshop_reader::QueryCollectionResult>, workshop_reader::error::Error> {
        let query = Self::build_collection_browse_url(app_id, page, params);
        if let Some(result) = self.cache.get_collection_search(query.as_str()) {
            Ok(result)
        } else {
            let page = self.request_page_wrapped(query.clone()).await?;
            Self::parse_browse_collection(&page)
                .map(Arc::new)
                .inspect(|arc| {
                    self.cache
                        .set_collection_search(query.as_str(), arc.clone());
                })
        }
    }
}

pub struct WorkshopBrowser<C, I>
where
    C: CacheProvider + Sync,
    I: Sized,
{
    client: WorkshopClient<C>,

    max_page: u32,
    page: u32,
    query: workshop_reader::QueryParams,

    edit_query: workshop_reader::QueryParams,
    edit_period: workshop_reader::QueryPeriod,
    scroll_id: iced::widget::Id,
    tags_toggled: bool,
    query_result: Option<Arc<workshop_reader::QueryResult<I>>>,
}

#[derive(Debug, Clone)]
pub enum WorkshopClientMessage<I> {
    Page(u32),
    SubmitQuery,
    RefreshQuery,
    EditQueryText(String),
    EditSort(web::WorkshopSort),
    EditPeriod(web::WorkshopTrendPeriod),
    ToggleTags(bool),
    EditTag(web::XCOM2WorkshopTag),

    ResolveQuery(Arc<workshop_reader::QueryResult<I>>),
}

impl From<WorkshopClientMessage<workshop_reader::QueryItem>> for crate::Message {
    fn from(value: WorkshopClientMessage<workshop_reader::QueryItem>) -> Self {
        Self::WorkshopMessageBrowseItemNoAPI(value)
    }
}

impl From<WorkshopClientMessage<workshop_reader::QueryCollection>> for crate::Message {
    fn from(value: WorkshopClientMessage<workshop_reader::QueryCollection>) -> Self {
        Self::WorkshopMessageBrowseCollectionNoAPI(value)
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

trait WorkshopBrowserNoAPI {
    type Item;

    fn render_preview_box_noapi<'a>(
        &'a self,
        state: &'a crate::App,
        item: &'a Self::Item,
    ) -> iced::Element<'a, crate::Message>;

    fn make_grid_noapi(&self) -> iced::widget::Grid<'_, crate::Message> {
        use iced::widget::grid;
        grid::Grid::new()
            .spacing(16)
            .fluid(360)
            .height(grid::Sizing::AspectRatio(0.5))
    }
}

impl<C, I> crate::browser::WorkshopBrowser for WorkshopBrowser<C, I>
where
    C: CacheProvider + Sync,
    I: Sized + Clone,
    WorkshopClientMessage<I>: Into<crate::Message>,
    Self: WorkshopBrowserNoAPI<Item = I>,
{
    type Item = I;
    type Message = WorkshopClientMessage<I>;

    fn get_max_page(&self) -> u32 {
        self.max_page
    }

    fn get_page(&self) -> u32 {
        self.page
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
        WorkshopClientMessage::Page(new)
    }

    fn on_query_period_edited(
        &self,
        _state: &crate::App,
        new: crate::web::WorkshopTrendPeriod,
    ) -> Self::Message {
        WorkshopClientMessage::EditPeriod(new)
    }

    fn on_query_sort_edited(&self, _state: &crate::App, new: web::WorkshopSort) -> Self::Message {
        WorkshopClientMessage::EditSort(new)
    }

    fn on_query_submitted(&self, _state: &crate::App) -> Self::Message {
        WorkshopClientMessage::SubmitQuery
    }

    fn on_query_tag_edited(
        &self,
        _state: &crate::App,
        tag: web::XCOM2WorkshopTag,
    ) -> Self::Message {
        WorkshopClientMessage::EditTag(tag)
    }

    fn on_query_tags_toggled(&self, _state: &crate::App, toggled: bool) -> Self::Message {
        WorkshopClientMessage::ToggleTags(toggled)
    }

    fn on_query_text_edited(&self, _state: &crate::App, new: String) -> Self::Message {
        WorkshopClientMessage::EditQueryText(new)
    }

    fn render_preview_box<'a>(
        &'a self,
        state: &'a crate::App,
        item: &'a Self::Item,
    ) -> iced::Element<'a, crate::Message> {
        self.render_preview_box_noapi(state, item)
    }

    fn make_grid(&self) -> iced::widget::Grid<'_, crate::Message> {
        self.make_grid_noapi()
    }
}

impl<C> WorkshopBrowserNoAPI for WorkshopBrowser<C, workshop_reader::QueryItem>
where
    C: CacheProvider + Sync,
{
    type Item = workshop_reader::QueryItem;

    fn render_preview_box_noapi<'a>(
        &'a self,
        state: &'a crate::App,
        item: &'a workshop_reader::QueryItem,
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
            text(star_string(item.stars)),
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

impl<C> WorkshopBrowserNoAPI for WorkshopBrowser<C, workshop_reader::QueryCollection>
where
    C: CacheProvider + Sync,
{
    type Item = workshop_reader::QueryCollection;

    fn render_preview_box_noapi<'a>(
        &'a self,
        state: &'a crate::App,
        item: &'a workshop_reader::QueryCollection,
    ) -> iced::Element<'a, crate::Message> {
        use iced::{
            Length::Fill,
            widget::{button, column, container, row, text, tooltip},
        };

        let id = item.id;

        tooltip(
            button(row![
                web::image_maybe(&state.images, &item.preview_url)
                    .width(128)
                    .height(Fill),
                column![
                    text(&item.title),
                    text(id),
                    text(&item.assembler_name),
                    text(star_string(item.stars)),
                ]
            ])
            .on_press(crate::Message::SetViewingScrapedCollection(id))
            .style(button::text),
            item.short_description.as_ref().map(|desc| {
                container(text(desc).shaping(text::Shaping::Advanced))
                    .padding(8)
                    .style(container::rounded_box)
            }),
            tooltip::Position::Bottom,
        )
        .into()
    }

    fn make_grid_noapi(&self) -> iced::widget::Grid<'_, crate::Message> {
        use iced::widget::grid;
        grid::Grid::new()
            .fluid(512)
            .spacing(16)
            .height(grid::aspect_ratio(640, 160))
    }
}

// TODO: Just... don't do this...
// I only realized close to the end that I didnt't have a way of discriminating
pub enum QueryType {
    Item,
    Collection,
}

pub trait Queryable {
    const QUERY_TYPE: QueryType;
}

impl Queryable for workshop_reader::QueryItem {
    const QUERY_TYPE: QueryType = QueryType::Item;
}

impl Queryable for workshop_reader::QueryCollection {
    const QUERY_TYPE: QueryType = QueryType::Collection;
}

impl<I> WorkshopBrowser<DefaultCacheProvider, I>
where
    I: workshop_reader::ItemPreviewImage + Queryable,
{
    pub fn new(client: WorkshopClient<DefaultCacheProvider>) -> Self {
        Self {
            client,
            query: Default::default(),
            page: 0,
            max_page: 0,
            edit_query: Default::default(),
            edit_period: Default::default(),
            scroll_id: iced::widget::Id::unique(),
            tags_toggled: false,
            query_result: None,
        }
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

    pub fn set_lifetime(&mut self, lifetime: u64) {
        let lifetime = if lifetime > 0 {
            lifetime
        } else {
            crate::web::DEFAULT_CACHE_TIME as u64
        };
        self.client
            .lifetime
            .store(lifetime, std::sync::atomic::Ordering::Relaxed);
        self.client.webcache.set_lifetime(lifetime);
    }

    pub fn get_items(&self) -> impl Iterator<Item = &I> {
        self.query_result
            .as_ref()
            .map(|query| query.items.iter())
            .into_iter()
            .flatten()
    }

    pub fn request_item_details(
        &self,
        id: u64,
    ) -> impl Future<
        Output = Result<Arc<workshop_reader::WorkshopFile>, workshop_reader::error::Error>,
    > + 'static {
        let client = self.client.clone();
        Box::pin(async move { client.request_item_details(id).await })
    }

    pub fn request_collection_details(
        &self,
        id: u64,
    ) -> impl Future<
        Output = Result<Arc<workshop_reader::WorkshopCollection>, workshop_reader::error::Error>,
    > + 'static {
        let client = self.client.clone();
        Box::pin(async move { client.request_collection_details(id).await })
    }

    pub fn update(
        &mut self,
        images: &HashMap<String, iced::widget::image::Handle>,
        message: WorkshopClientMessage<I>,
    ) -> Task<crate::Message> {
        match message {
            WorkshopClientMessage::Page(page) => {
                let new = std::cmp::max(page, 1);
                if new == self.page {
                    return Task::none();
                }
                self.page = new;

                return self.update(images, WorkshopClientMessage::RefreshQuery);
            }
            WorkshopClientMessage::SubmitQuery => {
                if let workshop_reader::QuerySort::Trend(period) = &mut self.edit_query.sort_method
                {
                    *period = self.edit_period;
                }
                self.page = 1;
                self.query = self.edit_query.clone();

                return self.update(images, WorkshopClientMessage::RefreshQuery);
            }
            WorkshopClientMessage::RefreshQuery => {
                let query = self.query.clone();
                let page = self.page;
                let client = self.client.clone();

                let scroll_task = reset_scroll!(self.scroll_id.clone());
                return Task::done(crate::Message::SetBusy(true))
                    .chain(Task::future(async move {
                        let res = match I::QUERY_TYPE {
                            QueryType::Collection => client
                                .query_collections(XCOM_APPID as u64, page, query)
                                .await
                                .map(|q| {
                                    crate::Message::from(WorkshopClientMessage::ResolveQuery(q))
                                }),
                            QueryType::Item => client
                                .query_items(XCOM_APPID as u64, page, query)
                                .await
                                .map(|q| {
                                    crate::Message::from(WorkshopClientMessage::ResolveQuery(q))
                                }),
                        };

                        match res {
                            Ok(query) => query,
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
                    let url = item.get_preview_url();
                    images.contains_key(&url).not().then(|| {
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

                self.max_page = resolved.pages;
                self.query_result = Some(resolved);
                return image_task;
            }
        }

        Task::none()
    }
}

impl App {
    pub fn set_viewing_collection_scraped(&mut self, id: u64) -> Task<crate::Message> {
        let existing: HashSet<_> = self.images.keys().cloned().collect();

        Task::future(self.noapi_collection_browser.request_collection_details(id)).then(move |resp| {
            match resp {
                Ok(details) => {
                    let images: Vec<_> = details.preview_url.iter()
                        .cloned()
                        .chain(
                            details.items.iter()
                                .filter_map(|i| i.preview_url.clone().filter(|url| !existing.contains(url)))
                            )
                        .collect();

                    Task::batch([
                        details.description.as_ref().map(|desc| {
                            match workshop_reader::descriptions::process_description_str(desc) {
                                Ok(items) => Task::done(crate::Message::ScrapedMarkupProcessed(details.id, items)),
                                Err(err) => {
                                    eprintln!("Error processing description: {err:?}");
                                    Task::none()
                                }
                            }
                        }).unwrap_or_default(),
                        Task::stream(iced::stream::channel(
                            64,
                            |mut sender: iced::futures::channel::mpsc::Sender<crate::Message>| async move {
                                for image in images.into_iter() {
                                    match web::load_image(&image).await {
                                        Ok(handle) => {
                                            if let Err(err) = sender.feed(crate::Message::ImageLoaded(image, handle)).await {
                                                eprintln!("Error sending resolved image: {err:?}");
                                            }
                                        }
                                        Err(err) => eprintln!("Error resolving image: {err:?}")
                                    }
                                }
                            },
                        ))
                    ])
                }
                Err(err) => Task::done(crate::Message::display_error(
                    "Error Loading Collection",
                    format!("Failed to get details for collection: {err:?}"),
                )),
            }
        })
    }

    pub fn view_collection_scraped(&self, id: u64) -> iced::Element<'_, crate::Message> {
        use crate::Message;
        use iced::{
            Center, Fill, Shrink,
            widget::{button, column, container, grid, markdown, row, scrollable, space, text},
        };

        let Some(details) = self.noapi_collection_browser.cache().get_collection(id) else {
            return container(
                column![
                    space::vertical(),
                    iced_aw::Spinner::new(),
                    text("Loading item details..."),
                    space::vertical(),
                    row![
                        space::horizontal(),
                        button("Close")
                            .style(button::danger)
                            .on_press(Message::CloseModal)
                    ],
                ]
                .align_x(Center)
                .height(Fill)
                .width(Fill),
            )
            .style(container::rounded_box)
            .into();
        };

        container(
            container(column![
                scrollable(
                    column![
                        column![
                            details.preview_url.as_ref().map(|url| {
                                web::image_maybe_fit(&self.images, url, iced::ContentFit::Cover)
                                    .height(300)
                                    .width(Fill)
                            }),
                            text(details.title.clone()),
                            text!("ID: {}", details.id),
                            text!("Items: {}", details.items.len()),
                            row![
                                button("Download All").on_press_with({
                                    let details = details.clone();
                                    move || {
                                        Message::DownloadMultipleRequested(
                                            details.items.iter().map(|i| i.id as u32).collect(),
                                        )
                                    }
                                }),
                                button("Add All to Profile").on_press_with({
                                    let collection = details.clone();
                                    move || {
                                        Message::ItemDetailsAddToLibraryRequest(Vec::from_iter(
                                            collection.items.iter().map(|i| (i.id as u32).into()),
                                        ))
                                    }
                                }),
                                button("Import as Profile").on_press_with({
                                    let collection = details.clone();
                                    move || {
                                        use crate::collections;
                                        // TODO: Resolve this properly instead of converting here
                                        let collection = collections::Collection {
                                            source: collections::CollectionSource::Workshop(
                                                collection.id as u32,
                                            ),
                                            title: collection.title.clone(),
                                            items: Vec::from_iter(
                                                collection.items.iter().map(|i| i.id as u32),
                                            ),
                                            image: collections::ImageSource::Web(
                                                collection.preview_url.clone().unwrap_or_default(),
                                            ),
                                            banner: None,
                                            description: String::new(),
                                        };
                                        Message::ProfileImportCollectionRequested(Arc::new(
                                            collection,
                                        ))
                                    }
                                }),
                            ]
                        ]
                        .width(Fill),
                        container({
                            self.markup_cache.get_scraped(id).map(|items| {
                                let settings = markdown::Settings::with_style(
                                    markdown::Style::from_palette(self.theme().palette()),
                                );

                                row(items.iter().map(|item| {
                                    crate::markup::view_scraped(item, settings, web::handle_url)
                                }))
                                .wrap()
                            })
                        })
                        .width(Fill)
                        .padding(16)
                        .style(container::dark),
                        grid(details.items.iter().map(|item| {
                            container(
                                button(
                                    row![
                                        web::image_maybe(
                                            &self.images,
                                            item.preview_url.clone().unwrap_or_default()
                                        ),
                                        column![
                                            text(item.title.clone()),
                                            text(item.id),
                                            item.short_description.clone().map(text)
                                        ]
                                        .clip(true)
                                    ]
                                    .spacing(8),
                                )
                                .style(button::text)
                                .on_press(Message::SetViewingItem((item.id as u32).into())),
                            )
                            .style(container::secondary)
                            .into()
                        }))
                        .fluid(300)
                        .spacing(16)
                        .height(grid::aspect_ratio(300, 100))
                    ]
                    .spacing(16)
                    .padding(16)
                    .height(Shrink)
                ),
                row![
                    space::horizontal(),
                    button("Close")
                        .style(button::danger)
                        .on_press(Message::CloseModal)
                ]
                .height(30)
            ])
            .style(container::rounded_box),
        )
        .center(Fill)
        .width(Fill)
        .padding(32)
        .into()
    }
}
