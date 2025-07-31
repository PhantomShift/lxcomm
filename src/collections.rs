use std::{collections::BTreeMap, fmt::Display, path::PathBuf, sync::Arc};

use iced::{
    Element,
    Length::{Fill, Shrink},
    Task,
    futures::SinkExt,
    widget::{
        button, column, container, grid, horizontal_space, image, markdown, row, scrollable, text,
    },
};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use steam_rs::Steam;

use crate::{
    App, LOCAL_COLLECTIONS_DIR, Message,
    browser::WorkshopBrowser,
    extensions::DetailsExtension,
    files::ModDetails,
    reset_scroll,
    web::{self, CollectionQueryResponse, ResolverMessage},
    xcom_mod::ModId,
};

#[derive(Debug, Clone, Hash, Serialize, Deserialize)]
pub enum ImageSource {
    Web(String),
    Path(PathBuf),
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CollectionSource {
    Workshop(u32),
    Local(PathBuf),
}

impl Display for CollectionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(path) => path.display().fmt(f),
            Self::Workshop(id) => id.fmt(f),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub source: CollectionSource,
    pub title: String,
    pub items: Vec<u32>,
    pub image: ImageSource,
    pub banner: Option<ImageSource>,
    pub description: String,
}

#[derive(Debug, Clone, Default)]
pub enum CollectionsMessage {
    QueryCollectionsLoaded(CollectionQueryResponse),
    SetBrowsePage(u32),
    BrowseSubmitQuery,
    BrowseUpdateQuery,
    BrowseEditQuery(String),
    BrowseEditSort(web::WorkshopSort),
    BrowseEditPeriod(web::WorkshopTrendPeriod),
    BrowseEditTag(web::XCOM2WorkshopTag),
    BrowseToggleTagsDropdown(bool),

    SetViewingCollection(CollectionSource),
    #[default]
    None,
}

impl PartialEq for Collection {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl PartialOrd for Collection {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.source.cmp(&other.source))
    }
}

impl From<CollectionsMessage> for Message {
    fn from(value: CollectionsMessage) -> Self {
        Self::Collections(value)
    }
}

#[derive(Debug)]
struct CollectionCache {
    web_cache: moka::sync::Cache<u32, Arc<Collection>>,
}

impl CollectionCache {
    fn get_workshop(&self, id: u32) -> Option<Arc<Collection>> {
        self.web_cache.get(&id)
    }
    fn insert_workshop(&self, id: u32, collection: &Collection) {
        if self.get_workshop(id).is_none() {
            self.web_cache.insert(id, Arc::new(collection.clone()));
        }
    }
}

impl Default for CollectionCache {
    fn default() -> Self {
        Self {
            web_cache: moka::sync::Cache::new(64),
        }
    }
}

#[derive(Debug)]
pub struct CollectionsState {
    web_cache: CollectionCache,
    pub loaded_web_collections: Arc<[Collection]>,
    local_collections: BTreeMap<PathBuf, Arc<Collection>>,
    browsing_scroll_id: iced::widget::scrollable::Id,
    browsing_page: u32,
    browsing_page_max: u32,
    browsing_query: web::WorkshopQuery,
    browsing_query_period: web::WorkshopTrendPeriod,
    browse_query_tags_open: bool,
}

impl Default for CollectionsState {
    fn default() -> Self {
        Self {
            web_cache: CollectionCache::default(),
            loaded_web_collections: Arc::new([]),
            local_collections: BTreeMap::new(),
            browsing_scroll_id: iced::widget::scrollable::Id::unique(),
            browsing_page: 0,
            browsing_page_max: 0,
            browsing_query: web::WorkshopQuery::default(),
            browsing_query_period: web::WorkshopTrendPeriod::default(),
            browse_query_tags_open: false,
        }
    }
}

impl WorkshopBrowser for CollectionsState {
    type Item = Collection;
    type Message = CollectionsMessage;

    fn get_max_page(&self) -> u32 {
        self.browsing_page_max
    }
    fn get_page(&self) -> u32 {
        self.browsing_page
    }
    fn get_query(&self) -> &web::WorkshopQuery {
        &self.browsing_query
    }
    fn get_tags_toggled(&self) -> bool {
        self.browse_query_tags_open
    }
    fn get_scroll_id(&self) -> iced::widget::scrollable::Id {
        self.browsing_scroll_id.clone()
    }
    fn on_page_change(&self, _state: &App, new: u32) -> Self::Message {
        CollectionsMessage::SetBrowsePage(new)
    }
    fn on_query_period_edited(&self, _state: &App, new: web::WorkshopTrendPeriod) -> Self::Message {
        CollectionsMessage::BrowseEditPeriod(new)
    }
    fn on_query_sort_edited(&self, _state: &App, new: web::WorkshopSort) -> Self::Message {
        CollectionsMessage::BrowseEditSort(new)
    }
    fn on_query_submitted(&self, _state: &App) -> Self::Message {
        CollectionsMessage::BrowseSubmitQuery
    }
    fn on_query_text_edited(&self, _state: &App, new: String) -> Self::Message {
        CollectionsMessage::BrowseEditQuery(new)
    }
    fn on_query_tag_edited(&self, _state: &App, tag: web::XCOM2WorkshopTag) -> Self::Message {
        CollectionsMessage::BrowseEditTag(tag)
    }
    fn on_query_tags_toggled(&self, _state: &App, toggled: bool) -> Self::Message {
        CollectionsMessage::BrowseToggleTagsDropdown(toggled)
    }
    fn render_preview_box<'a>(
        &'a self,
        state: &'a App,
        collection: &'a Self::Item,
    ) -> Element<'a, Message> {
        let image = match &collection.image {
            ImageSource::Web(source) => {
                // Apparently not having an image is pretty common...?
                if source.is_empty() {
                    container("NO IMAGE AVAILABLE").center(Fill)
                } else {
                    web::image_maybe(&state.images, source)
                }
            }
            ImageSource::Path(path) => container(image(path)),
        }
        .width(150)
        .height(Fill);
        button(
            row![
                image,
                column![
                    text(&collection.title).shaping(text::Shaping::Advanced),
                    text(collection.source.to_string()),
                    text(&collection.description).shaping(text::Shaping::Advanced),
                ],
            ]
            .spacing(16)
            .clip(true),
        )
        .on_press(CollectionsMessage::SetViewingCollection(collection.source.clone()).into())
        .style(button::text)
        .into()
    }

    fn make_grid(&self) -> iced::widget::Grid<'_, Message> {
        grid::Grid::new()
            .fluid(512)
            .spacing(16)
            .height(grid::aspect_ratio(640, 160))
    }
}

impl CollectionsState {
    // TODO - Implement saving and creating local collections
    pub fn load_local_collections(&mut self) -> Result<(), std::io::Error> {
        self.local_collections.clear();
        for entry in std::fs::read_dir(LOCAL_COLLECTIONS_DIR.as_path())? {
            let entry = entry?;
            let path = entry.path();
            if let Ok(contents) = std::fs::read_to_string(&path)
                && let Ok(local) = serde_json::from_str(&contents)
            {
                self.local_collections.insert(path, Arc::new(local));
            }
        }
        Ok(())
    }

    pub fn view_collection_detailed<'a>(
        &'a self,
        state: &'a App,
        source: &'a CollectionSource,
    ) -> Element<'a, Message> {
        let Some(collection) = (match source {
            CollectionSource::Local(path) => self.local_collections.get(path).cloned(),
            CollectionSource::Workshop(id) => self.web_cache.get_workshop(*id),
        }) else {
            return container("Collection not found...")
                .center(Fill)
                .style(container::rounded_box)
                .into();
        };

        container(
            container(column![
                // Fow some reason it won't render if it's at the bottom...?
                row![
                    horizontal_space(),
                    button("Close")
                        .style(button::danger)
                        .on_press(Message::CloseModal)
                ]
                .height(30),
                scrollable(
                    column![]
                        .push(collection.banner.as_ref().map(|source| {
                            web::image_from_source(&state.images, source.clone())
                                .width(Fill)
                                .padding(16)
                        }))
                        .push(row![
                            web::image_from_source(&state.images, collection.image.clone())
                                .max_height(300)
                                .max_width(300),
                            {
                                let collection = collection.clone();
                                column![
                                    text(collection.title.clone()).shaping(text::Shaping::Advanced),
                                    text(format!("Source: {}", collection.source)),
                                    row![
                                        button("Download All").on_press_with({
                                            let collection = collection.clone();
                                            move || match collection.source {
                                                CollectionSource::Workshop(_) => {
                                                    Message::DownloadMultipleRequested(
                                                        collection.items.clone(),
                                                    )
                                                }
                                                _ => Message::None,
                                            }
                                        }),
                                        button("Add All to Profile").on_press_with({
                                            let collection = collection.clone();
                                            move || {
                                                Message::ItemDetailsAddToLibraryRequest(
                                                    collection
                                                        .items
                                                        .iter()
                                                        .map(ModId::from)
                                                        .collect(),
                                                )
                                            }
                                        }),
                                        button("Import as Profile").on_press(
                                            Message::ProfileImportCollectionRequested(
                                                collection.clone()
                                            )
                                        )
                                    ],
                                ]
                                .height(Shrink)
                            }
                        ])
                        .push(
                            container(
                                markdown(
                                    state
                                        .markup_cache
                                        .get_markup(&collection.description)
                                        .unwrap_or_default(),
                                    markdown::Settings::with_style(state.theme().palette()),
                                )
                                .map(web::handle_url),
                            )
                            .width(Fill)
                            .padding(16)
                            .style(container::dark),
                        )
                        .push(
                            grid(collection.items.iter().map(|id| {
                                if let Some(ModDetails::Workshop(file)) =
                                    state.file_cache.get_details(ModId::Workshop(*id))
                                {
                                    container(
                                        button(
                                            row![
                                                web::image_maybe(&state.images, &file.preview_url),
                                                column![
                                                    text!("{}", file.title),
                                                    text(id.to_string()),
                                                    text(file.get_description().to_string())
                                                ]
                                                .clip(true)
                                            ]
                                            .spacing(8),
                                        )
                                        .style(button::text)
                                        .on_press(Message::SetViewingItem((*id).into())),
                                    )
                                    .style(container::secondary)
                                    .into()
                                } else {
                                    text!("UNKNOWN ({id})").into()
                                }
                            }))
                            .fluid(300)
                            .spacing(16)
                            .height(grid::aspect_ratio(300, 100)),
                        )
                        .spacing(16)
                        .padding(16)
                        .height(Shrink),
                )
                .height(Shrink),
            ])
            .style(container::rounded_box),
        )
        .center(Fill)
        .width(Fill)
        .padding(32)
        .into()
    }
}

impl App {
    pub fn update_collections(&mut self, message: CollectionsMessage) -> Task<Message> {
        // holy moly please internal mutability
        // if you're reading this and you know of a better
        // way of doing aliases please for the love of god tell me
        macro_rules! state {
            () => {
                self.collections
            };
        }

        match message {
            CollectionsMessage::QueryCollectionsLoaded(CollectionQueryResponse {
                total,
                collections,
            }) => {
                state!().loaded_web_collections = collections.clone();
                state!().browsing_page_max = total as u32 / web::QUERY_PAGE_SIZE + 1;

                let mut resolve_images = Task::none();
                for collection in collections.iter() {
                    if let CollectionSource::Workshop(id) = collection.source {
                        state!().web_cache.insert_workshop(id, collection);
                    }
                    if let ImageSource::Web(source) = &collection.image
                        && !source.is_empty()
                    {
                        resolve_images = resolve_images.chain(self.cache_item_image(source));
                    }
                    if self
                        .markup_cache
                        .get_markup(&collection.description)
                        .is_none()
                    {
                        self.markup_cache.cache_markup(&collection.description);
                    }
                }

                return resolve_images;
            }

            CollectionsMessage::SetBrowsePage(page) => {
                let new = std::cmp::max(page, 1);
                if new == state!().browsing_page {
                    return Task::none();
                }

                state!().browsing_page = new;
                return Task::done(CollectionsMessage::BrowseUpdateQuery.into());
            }
            CollectionsMessage::BrowseEditPeriod(period) => {
                state!().browsing_query_period = period;
                if let web::WorkshopSort::Trend(old) = &mut state!().browsing_query.sort {
                    *old = period;
                }
            }
            CollectionsMessage::BrowseEditQuery(query) => {
                state!().browsing_query.query = query;
            }
            CollectionsMessage::BrowseEditSort(sort) => {
                state!().browsing_query.sort = sort;
            }
            CollectionsMessage::BrowseEditTag(tag) => {
                if state!().browsing_query.tags.contains(&tag) {
                    state!().browsing_query.tags.remove(&tag);
                } else {
                    state!().browsing_query.tags.insert(tag);
                }
            }
            CollectionsMessage::BrowseToggleTagsDropdown(toggled) => {
                state!().browse_query_tags_open = toggled;
            }
            CollectionsMessage::BrowseSubmitQuery => {
                state!().browsing_page = 1;

                return Task::done(CollectionsMessage::BrowseUpdateQuery.into());
            }
            CollectionsMessage::BrowseUpdateQuery => {
                let page = state!().browsing_page;
                let query = state!().browsing_query.clone();
                let api_key = self.api_key.clone();
                let scroll_task = reset_scroll!(state!().browsing_scroll_id.clone());
                return Task::done(Message::SetBusy(true))
                    .chain(Task::future(async move {
                        match web::query_collections(
                            Steam::new(api_key.expose_secret()),
                            page,
                            query,
                        )
                        .await
                        {
                            Ok(response) => {
                                CollectionsMessage::QueryCollectionsLoaded(response).into()
                            }
                            Err(e) => Message::display_error("Page Load Failed", e.to_string()),
                        }
                    }))
                    .chain(scroll_task)
                    .chain(Task::done(Message::SetBusy(false)));
            }
            CollectionsMessage::SetViewingCollection(source) => {
                if let CollectionSource::Workshop(id) = source
                    && let Some(collection) = state!().web_cache.get_workshop(id)
                    && let Some(mut sender) = self.background_resolver_sender.clone()
                {
                    let resolve_images = collection
                        .items
                        .iter()
                        .filter_map(|id| {
                            self.file_cache
                                .get_details(ModId::from(*id))
                                .and_then(ModDetails::maybe_workshop)
                        })
                        .fold(Task::none(), |task, file| {
                            task.chain(self.cache_item_image(&file.preview_url))
                        });

                    self.modal_stack
                        .push(crate::AppModal::CollectionDetailedView(source.clone()));
                    let unknown = collection
                        .items
                        .iter()
                        .filter(|id| self.file_cache.get_details(ModId::from(**id)).is_none())
                        .copied()
                        .collect::<Vec<_>>();
                    return if !unknown.is_empty() {
                        Task::batch([
                            Task::future(async move {
                                if let Err(err) = sender
                                    .send(ResolverMessage::RequestResolve(unknown).into())
                                    .await
                                {
                                    eprintln!(
                                        "Error sending resolve request to background task: {err:?}"
                                    );
                                }
                                Message::None
                            }),
                            resolve_images,
                        ])
                    } else {
                        resolve_images
                    };
                }
            }

            CollectionsMessage::None => {}
        }

        Task::none()
    }
}
