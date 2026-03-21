use std::{
    collections::BTreeSet,
    sync::{Arc, LazyLock},
};

use chrono::{Datelike, NaiveDateTime};
use scraper::{Element, ElementRef};
use serde::{Deserialize, Serialize};
use strum::VariantArray;

#[macro_use]
extern crate markup5ever;

pub mod descriptions;

pub mod selectors {
    use std::sync::LazyLock;

    use scraper::Selector;

    macro_rules! selector {
        ($name:ident, $sel:expr) => {
            pub static $name: LazyLock<Selector> =
                LazyLock::new(|| Selector::parse($sel).expect("selector should parse"));
        };
    }

    // Workshop item page selectors
    selector!(DETAILS_STATS_RIGHT, ".detailsStatRight");
    selector!(PREVIEW_IMAGE, "#previewImage");
    selector!(PREVIEW_IMAGE_MAIN, "#previewImageMain");
    selector!(ITEM_TITLE, ".workshopItemTitle");
    selector!(BREADCRUMBS, ".breadcrumbs");
    selector!(DESCRIPTION, ".workshopItemDescription");
    selector!(STATS_TABLE, ".stats_table");
    selector!(REQUIRED_ITEMS, "#RequiredItems");
    selector!(RIGHT_DETAILS_BLOCK, ".rightDetailsBlock");
    selector!(FILE_RATING_DETAILS, ".fileRatingDetails");
    selector!(WORKSHOP_TAGS, ".workshopTags");

    // Workshop item browse page selectors
    selector!(BROWSE_ITEMS, ".workshopBrowseItems");
    selector!(AUTHOR_LINK, ".workshop_author_link");
    selector!(ITEM_PREVIEW_IMAGE, ".workshopItemPreviewImage ");
    selector!(FILE_RATING, ".fileRating");
    selector!(UGC, ".ugc");
    selector!(NO_ITEMS, "#no_items");
    selector!(PAGING_INFO, ".workshopBrowsePagingInfo");

    // Workshop collection page selectors
    selector!(COLLECTION_ITEM, ".collectionItem");
    selector!(COLLECTION_ITEM_DETAILS, ".collectionItemDetails");
    // Assembler name (there is only one assembler, unlike
    // the situation with items which can have multiple authors)
    selector!(FRIEND_BLOCK_CONTENT, ".friendBlockContent");
    // Image
    selector!(COLLECTION_BACKGROUND_IMAGE, "#CollectionBackgroundImage");
    // Posted and updated time
    selector!(RIGHT_DETAILS_CONTAINER, ".detailsStatsContainerRight");

    // Workshop collection item selectors
    // Author span containing anchor with name
    selector!(WORKSHOP_AUTHOR_NAME, ".workshopItemAuthorName");
    selector!(WORKSHOP_SHORT_DESC, ".workshopItemShortDesc");

    // Workshop collection browse page selectors
    selector!(WORKSHOP_ITEM_COLLECTION, ".workshopItemCollection");
}

pub mod error {
    #[derive(Debug)]
    pub enum ErrorKind {
        ChronoParseError(chrono::ParseError),
        RequestError(Box<dyn std::error::Error + Send>),
        MissingFile,
        ParseError,
    }

    #[derive(Debug)]
    pub struct Error {
        kind: ErrorKind,
        message: Option<String>,
    }

    impl Error {
        pub fn new(kind: ErrorKind) -> Self {
            Self {
                kind,
                message: None,
            }
        }

        pub fn msg<S: Into<String>>(self, s: S) -> Self {
            Self {
                message: Some(s.into()),
                ..self
            }
        }

        pub fn parse_error<S: Into<String>>(s: S) -> Self {
            Self {
                kind: ErrorKind::ParseError,
                message: Some(s.into()),
            }
        }
    }

    impl<T> From<T> for Error
    where
        T: AsRef<str>,
    {
        fn from(value: T) -> Self {
            Self::parse_error(value.as_ref().to_owned())
        }
    }

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match &self.kind {
                ErrorKind::ChronoParseError(err) => {
                    writeln!(f, "Chrono Parse Error ({err})")
                }
                ErrorKind::RequestError(err) => {
                    writeln!(f, "Request Error ({err})")
                }
                ErrorKind::MissingFile => {
                    writeln!(f, "Missing File Error: The requested item does not exist")
                }
                ErrorKind::ParseError => {
                    if let Some(msg) = &self.message {
                        write!(f, "Parse Error: {msg}")
                    } else {
                        write!(f, "Parse Error")
                    }
                }
            }
        }
    }

    impl std::error::Error for Error {}
}

#[derive(Debug)]
pub struct WorkshopFile {
    pub published_file_id: u64,
    pub creator: String,
    pub file_size: String,
    pub preview_url: String,
    pub title: String,
    pub file_description: Option<String>,
    pub time_created: u64,
    pub time_updated: u64,
    pub subscriptions: u64,
    pub favorited: u64,
    pub children: Arc<[u64]>,
    pub tags: Arc<[String]>,
    pub score: u8,
}

/// Data stored for previewing items in a collection
#[derive(Debug)]
pub struct WorkshopCollectionItem {
    pub id: u64,
    pub title: String,
    pub author_name: String,
    pub short_description: Option<String>,
    pub preview_url: Option<String>,
    pub stars: u8,
}

#[derive(Debug)]
pub struct WorkshopCollection {
    pub id: u64,
    pub title: String,
    pub assembler_name: String,
    pub items: Arc<[WorkshopCollectionItem]>,
    pub preview_url: Option<String>,
    pub description: Option<String>,
    pub time_created: u64,
    pub time_updated: u64,
    pub stars: u8,
}

#[derive(Debug, Clone, Copy, Default, Hash, PartialEq, Eq, PartialOrd, Ord, VariantArray)]
pub enum QueryPeriod {
    Today,
    #[default]
    Week,
    #[strum(to_string = "Three Months")]
    ThreeMonths,
    #[strum(to_string = "Six Months")]
    SixMonths,
    #[strum(to_string = "One Year")]
    OneYear,
    #[strum(to_string = "All Time")]
    AllTime,
}

impl QueryPeriod {
    pub const fn as_days(&self) -> i32 {
        match self {
            Self::Today => 1,
            Self::Week => 7,
            Self::ThreeMonths => 90,
            Self::SixMonths => 180,
            Self::OneYear => 365,
            Self::AllTime => -1,
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum QuerySort {
    Trend(QueryPeriod),
    MostRecent,
    LastUpdated,
    TotalUniqueSubscribers,
    TextSearch,
}

impl Default for QuerySort {
    fn default() -> Self {
        Self::Trend(QueryPeriod::Week)
    }
}

impl QuerySort {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Trend(_) => "trend",
            Self::MostRecent => "mostrecent",
            Self::LastUpdated => "lastupdated",
            Self::TotalUniqueSubscribers => "totaluniquesubscribers",
            Self::TextSearch => "textsearch",
        }
    }

    const fn maybe_period(&self) -> Option<i32> {
        if let Self::Trend(period) = self {
            Some(period.as_days())
        } else {
            None
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct QueryParams {
    pub search_text: String,
    pub sort_method: QuerySort,
    pub tags: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct QueryItem {
    pub id: u64,
    pub stars: u8,
    pub title: String,
    pub author_name: String,
    pub author_id: String,
    pub preview_url: String,
    pub short_description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QueryCollection {
    pub id: u64,
    pub stars: u8,
    pub title: String,
    pub assembler_name: String,
    pub preview_url: String,
    pub short_description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QueryResult<I> {
    pub pages: u32,
    pub items: Arc<[I]>,
}

pub type QueryItemResult = QueryResult<QueryItem>;
pub type QueryCollectionResult = QueryResult<QueryCollection>;

pub trait ItemPreviewImage {
    fn get_preview_url(&self) -> String;
}

impl ItemPreviewImage for QueryItem {
    fn get_preview_url(&self) -> String {
        self.preview_url.clone()
    }
}

impl ItemPreviewImage for QueryCollection {
    fn get_preview_url(&self) -> String {
        self.preview_url.clone()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkshopHoverInfo {
    pub id: String,
    pub title: String,
    pub description: String,
    pub user_subscribed: bool,
    pub user_favorited: bool,
    pub played: bool,
    pub appid: u64,
}

pub trait PageProvider {
    type Error: std::error::Error + Send + 'static;

    fn build_item_url(id: u64) -> reqwest::Url {
        reqwest::Url::parse_with_params(
            "https://steamcommunity.com/sharedfiles/filedetails/",
            [("id", id.to_string().as_str()), ("l", "english")],
        )
        .expect("base url should be well formed")
    }

    fn build_browse_url(app_id: u64, page: u32, params: QueryParams) -> reqwest::Url {
        let mut base = reqwest::Url::parse("https://steamcommunity.com/workshop/browse/")
            .expect("base should be a well-formed URL");
        {
            let mut query = base.query_pairs_mut();
            query.append_pair("appid", &app_id.to_string());
            if !params.search_text.is_empty() {
                query.append_pair("searchtext", &params.search_text);
            }
            query.append_pair("browsesort", params.sort_method.as_str());
            query.append_pair("section", "readytouseitems");
            query.append_pair("p", &page.to_string());
            if let Some(period) = params.sort_method.maybe_period() {
                query.append_pair("days", &period.to_string());
            }
            query.append_pair("l", "english");
            query.finish();
        }

        base
    }

    fn build_collection_browse_url(app_id: u64, page: u32, params: QueryParams) -> reqwest::Url {
        let mut url = Self::build_browse_url(app_id, page, params);
        url.query_pairs_mut()
            .append_pair("section", "collections")
            .finish();
        url
    }

    fn parse_item(page: &str) -> Result<WorkshopFile, error::Error> {
        parse_document(scraper::Html::parse_document(page))
    }

    fn parse_browse(page: &str) -> Result<QueryItemResult, error::Error> {
        parse_browse_result(scraper::Html::parse_document(page))
    }

    fn parse_collection(page: &str) -> Result<WorkshopCollection, error::Error> {
        parse_collection_document(scraper::Html::parse_document(page))
    }

    fn parse_browse_collection(page: &str) -> Result<QueryCollectionResult, error::Error> {
        parse_collection_browse(scraper::Html::parse_document(page))
    }

    fn request_page<U: reqwest::IntoUrl + Send>(
        &self,
        url: U,
    ) -> impl std::future::Future<Output = Result<String, Self::Error>> + Send;

    fn request_page_wrapped<U: reqwest::IntoUrl + Send>(
        &self,
        url: U,
    ) -> impl std::future::Future<Output = Result<String, error::Error>> {
        async move {
            self.request_page(url)
                .await
                .map_err(|err| error::Error::new(error::ErrorKind::RequestError(Box::new(err))))
        }
    }

    fn request_item_details(
        &self,
        published_file_id: u64,
    ) -> impl std::future::Future<Output = Result<Arc<WorkshopFile>, error::Error>> {
        async move {
            let page = self
                .request_page_wrapped(Self::build_item_url(published_file_id))
                .await?;

            Self::parse_item(&page)
                .map(|f| WorkshopFile {
                    published_file_id,
                    ..f
                })
                .map(Arc::new)
        }
    }

    fn query_items(
        &self,
        app_id: u64,
        page: u32,
        params: QueryParams,
    ) -> impl std::future::Future<Output = Result<Arc<QueryItemResult>, error::Error>> {
        async move {
            let url = Self::build_browse_url(app_id, page, params);
            let page = self.request_page_wrapped(url).await?;
            Self::parse_browse(&page).map(Arc::new)
        }
    }

    fn request_collection_details(
        &self,
        id: u64,
    ) -> impl std::future::Future<Output = Result<Arc<WorkshopCollection>, error::Error>> {
        async move {
            let page = self.request_page_wrapped(Self::build_item_url(id)).await?;

            Self::parse_collection(&page)
                .map(|f| WorkshopCollection { id, ..f })
                .map(Arc::new)
        }
    }

    fn query_collections(
        &self,
        app_id: u64,
        page: u32,
        params: QueryParams,
    ) -> impl std::future::Future<Output = Result<Arc<QueryCollectionResult>, error::Error>> {
        async move {
            let page = self
                .request_page_wrapped(Self::build_collection_browse_url(app_id, page, params))
                .await?;

            Self::parse_browse_collection(&page).map(Arc::new)
        }
    }
}

/// Important note: assumes that the page language is in English
fn parse_time(s: &str) -> Result<u64, error::Error> {
    static YEAR_NOW: LazyLock<i32> = LazyLock::new(|| chrono::Local::now().year());
    NaiveDateTime::parse_from_str(&format!("{} {s}", *YEAR_NOW), "%Y %b %-d @ %-I:%M%P")
        .map(|date_time| {
            date_time
                .with_year(chrono::Local::now().year())
                .expect("Current year with current month and day should exist")
        })
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%b %-d, %Y @ %-I:%M%P"))
        .map_err(|err| {
            error::Error::new(error::ErrorKind::ChronoParseError(err))
                .msg("failed to parse timestamp")
        })
        .and_then(|dt| {
            dt.and_local_timezone(chrono::Local)
                .earliest()
                .ok_or(
                    error::Error::new(error::ErrorKind::ParseError)
                        .msg("failed to get time for local timezone"),
                )
                .map(|dt| dt.timestamp() as u64)
        })
}

fn parse_item_title(doc: &scraper::Html) -> Result<String, error::Error> {
    doc.select(&selectors::ITEM_TITLE)
        .next()
        .map(|e| e.inner_html())
        .ok_or_else(|| error::Error::parse_error("Missing workshop item title"))
}

fn find_item_description(doc: &scraper::Html) -> Option<String> {
    doc.select(&selectors::DESCRIPTION)
        .next()
        .map(|el| el.html())
}

fn parse_score_large(doc: &scraper::Html) -> u8 {
    doc.select(&selectors::FILE_RATING_DETAILS)
        .next()
        .and_then(|e| e.first_element_child())
        .and_then(|el| el.attr("src"))
        .and_then(|src| reqwest::Url::parse(src).ok())
        .and_then(|url| {
            url.path_segments()
                .into_iter()
                .flatten()
                .last()
                .map(|file_name| match file_name {
                    "5-star_large.png" => 5,
                    "4-star_large.png" => 4,
                    "3-star_large.png" => 3,
                    "2-star_large.png" => 2,
                    "1-star_large.png" => 1,
                    _ => 0,
                })
        })
        .unwrap_or(0)
}

fn parse_score_small(el: ElementRef<'_>) -> u8 {
    el.select(&selectors::FILE_RATING)
        .next()
        .and_then(|rating| rating.attr("src"))
        .and_then(|src| reqwest::Url::parse(src).ok())
        .and_then(|url| {
            url.path_segments()
                .into_iter()
                .flatten()
                .last()
                .map(|file_name| match file_name {
                    "5-star.png" => 5,
                    "4-star.png" => 4,
                    "3-star.png" => 3,
                    "2-star.png" => 2,
                    "1-star.png" => 1,
                    _ => 0,
                })
        })
        .unwrap_or(0)
}

fn parse_browse_pages(doc: &scraper::Html) -> Result<u32, error::Error> {
    let paging_info = doc
        .select(&selectors::PAGING_INFO)
        .next()
        .ok_or(error::Error::parse_error("failed to get paging info"))?
        .inner_html();

    let pages = paging_info
        .split_once(" of ")
        .ok_or(error::Error::parse_error("failed to get paging info"))?
        .1
        .replace(|c: char| !c.is_ascii_digit(), "")
        .parse::<u32>()
        .map_err(|_| error::Error::parse_error("failed to get paging info"))?
        / 30
        + 1;

    Ok(pages)
}

fn parse_document(doc: scraper::Html) -> Result<WorkshopFile, error::Error> {
    let creator = {
        let url = doc
            .select(&selectors::BREADCRUMBS)
            .next()
            .ok_or(error::Error::parse_error("missing breadcrumbs div"))?
            .child_elements()
            .last()
            .ok_or(error::Error::parse_error(
                "breadcrumbs is missing child elements",
            ))?
            .attr("href")
            .ok_or(error::Error::parse_error("workshop anchor is missing href"))?;
        let Ok(url) = reqwest::Url::parse(url) else {
            return Err(error::Error::parse_error("workshop url was invalid"));
        };
        url.path_segments()
            .into_iter()
            .flatten()
            .nth(1)
            .ok_or(error::Error::parse_error(
                "workshop url only had one path segment",
            ))?
            .to_string()
    };

    let stats_right: Vec<_> = doc.select(&selectors::DETAILS_STATS_RIGHT).collect();
    if !(2..=3).contains(&stats_right.len()) {
        return Err(error::Error::new(error::ErrorKind::ParseError)
            .msg("Unexpected number of stats contained in stats container"));
    }
    let file_size = stats_right[0].inner_html();
    let time_created = parse_time(&stats_right[1].inner_html())?;
    let time_updated = if stats_right.len() == 3 {
        parse_time(&stats_right[2].inner_html())?
    } else {
        time_created
    };

    let preview_image = doc
        .select(&selectors::PREVIEW_IMAGE)
        .next()
        .or_else(|| doc.select(&selectors::PREVIEW_IMAGE_MAIN).next())
        .ok_or(error::Error::parse_error(
            "Document is missing preview image",
        ))?;
    let preview_image_url = preview_image.attr("src").ok_or(error::Error::parse_error(
        "Preview image is missing its source",
    ))?;
    let mut preview_image_url = reqwest::Url::parse(preview_image_url)
        .map_err(|err| error::Error::parse_error(err.to_string()))?;
    preview_image_url.set_query(None);

    let title = parse_item_title(&doc)?;
    let file_description = find_item_description(&doc);

    let mut pop_stats = doc
        .select(&selectors::STATS_TABLE)
        .next()
        .map(|e| e.descendants())
        .into_iter()
        .flatten()
        .filter_map(|node| {
            node.value()
                .as_text()
                .and_then(|t| t.replace(',', "").parse::<u64>().ok())
        })
        .skip(1); // Skip unique viewers
    let subscriptions = pop_stats.next().unwrap_or(0);
    let favorited = pop_stats.next().unwrap_or(0);

    let children = doc
        .select(&selectors::REQUIRED_ITEMS)
        .next()
        .map(|e| e.child_elements())
        .into_iter()
        .flatten()
        .filter_map(|el| {
            el.attr("href").and_then(|s| {
                s.trim_start_matches(|c: char| !char::is_ascii_digit(&c))
                    .parse::<u64>()
                    .ok()
            })
        })
        .collect();

    let builtin_tags = doc
        .select(&selectors::RIGHT_DETAILS_BLOCK)
        .next()
        .map(|e| e.child_elements())
        .into_iter()
        .flatten()
        .filter_map(|el| (el.value().name() == "a").then_some(el.inner_html()));

    let custom_tags = doc
        .select(&selectors::WORKSHOP_TAGS)
        .next()
        .map(|e| e.child_elements())
        .into_iter()
        .flatten()
        .filter_map(|el| (el.value().name() == "a").then_some(el.inner_html()));

    let tags = Arc::from_iter(builtin_tags.chain(custom_tags).filter(|t| !t.is_empty()));

    let score = parse_score_large(&doc);

    Ok(WorkshopFile {
        published_file_id: 0,
        creator,
        file_size,
        preview_url: preview_image_url.to_string(),
        title,
        file_description,
        time_created,
        time_updated,
        subscriptions,
        favorited,
        children,
        tags,
        score,
    })
}

fn parse_browse_result(doc: scraper::Html) -> Result<QueryItemResult, error::Error> {
    if doc.select(&selectors::NO_ITEMS).next().is_some() {
        return Ok(QueryItemResult {
            pages: 0,
            items: Arc::new([]),
        });
    }

    let pages = parse_browse_pages(&doc)?;

    let Some(browse_items) = doc.select(&selectors::BROWSE_ITEMS).next() else {
        return Err(error::Error::parse_error("failed to find list of items"));
    };
    let mut items = Vec::new();
    let mut children = browse_items.child_elements();
    while let Some(item) = children.next()
        && let Some(script) = children.next()
    {
        let author_link =
            item.select(&selectors::AUTHOR_LINK)
                .next()
                .ok_or(error::Error::parse_error(
                    "workshop item missing author information",
                ))?;
        let author_id = author_link
            .attr("href")
            .and_then(|href| reqwest::Url::parse(href).ok())
            .and_then(|url| {
                url.path_segments()
                    .into_iter()
                    .flatten()
                    .nth(1)
                    .map(str::to_string)
            })
            .ok_or(error::Error::parse_error("failed to get author ID"))?;

        let short_description = script
            .inner_html()
            .find(r#"{"id":"#)
            .and_then(|i| {
                serde_json::from_str(
                    script
                        .inner_html()
                        .split_off(i)
                        .trim()
                        .trim_end_matches(");"),
                )
                .inspect_err(|e| eprintln!("Failed to parse hover json: {e:?}"))
                .ok()
            })
            .map(|h: WorkshopHoverInfo| h.description);

        items.push(QueryItem {
            id: item
                .select(&selectors::UGC)
                .next()
                .and_then(|ugc| ugc.attr("data-publishedfileid"))
                .ok_or(error::Error::parse_error("item missing id"))?
                .parse::<u64>()
                .map_err(|_| error::Error::parse_error("invalid id"))?,
            stars: parse_score_small(item),
            title: item
                .select(&selectors::ITEM_TITLE)
                .next()
                .ok_or(error::Error::parse_error("workshop item missing title"))?
                .inner_html(),
            author_name: author_link.inner_html(),
            author_id,
            short_description,
            preview_url: item
                .select(&selectors::ITEM_PREVIEW_IMAGE)
                .next()
                .and_then(|img| img.attr("src"))
                .map(|src| {
                    if let Ok(mut url) = reqwest::Url::parse(src) {
                        url.set_query(None);
                        url.to_string()
                    } else {
                        src.to_string()
                    }
                })
                .ok_or(error::Error::parse_error("missing preview url"))?,
        });
    }

    Ok(QueryItemResult {
        pages,
        items: items.into(),
    })
}

fn parse_collection_document(doc: scraper::Html) -> Result<WorkshopCollection, error::Error> {
    let title = parse_item_title(&doc)?;
    let description = find_item_description(&doc);

    let assembler_name = doc
        .select(&selectors::FRIEND_BLOCK_CONTENT)
        .next()
        .and_then(|el| el.text().next())
        .map(str::to_string)
        .ok_or(error::Error::parse_error("failed to parse assembler name"))?;

    let preview_url = doc
        .select(&selectors::COLLECTION_BACKGROUND_IMAGE)
        .next()
        .and_then(|el| el.attr("src"))
        .map(str::to_string);

    let details = doc
        .select(&selectors::RIGHT_DETAILS_CONTAINER)
        .next_back()
        .ok_or(error::Error::parse_error(
            "failed to find upload/update details",
        ))?;
    let mut details_iter = details.select(&selectors::DETAILS_STATS_RIGHT);
    let time_created = details_iter
        .next()
        .ok_or(error::Error::parse_error("failed to find post time"))
        .and_then(|el| parse_time(&el.inner_html()))?;
    let time_updated = details_iter
        .next()
        .map(|el| parse_time(&el.inner_html()))
        .unwrap_or(Ok(time_created))?;

    let stars = parse_score_large(&doc);

    let items = doc
        .select(&selectors::COLLECTION_ITEM)
        .map(parse_collection_item)
        .collect::<Result<Arc<_>, _>>()?;

    Ok(WorkshopCollection {
        id: 0,
        title,
        assembler_name,
        items,
        preview_url,
        description,
        time_created,
        time_updated,
        stars,
    })
}

fn parse_collection_item(el: ElementRef<'_>) -> Result<WorkshopCollectionItem, error::Error> {
    let url = el
        .select(&selectors::COLLECTION_ITEM_DETAILS)
        .next()
        .and_then(|d| d.first_element_child())
        .and_then(|a| a.attr("href"))
        .ok_or(error::Error::parse_error("failed to get id"))
        .map(reqwest::Url::parse)?
        .map_err(|_| error::Error::parse_error("failed to parse url for id"))?;
    let id = url
        .query_pairs()
        .find_map(|(k, v)| {
            if k == "id" {
                v.parse::<u64>().ok()
            } else {
                None
            }
        })
        .ok_or(error::Error::parse_error("failed to find id in url"))?;

    let title = el
        .select(&selectors::ITEM_TITLE)
        .next()
        .and_then(|t| t.text().next())
        .map(str::trim)
        .map(str::to_string)
        .ok_or(error::Error::parse_error("failed to find title"))?;

    let author_name = el
        .select(&selectors::WORKSHOP_AUTHOR_NAME)
        .next()
        .and_then(|a| a.text().next())
        .map(str::trim)
        .map(str::to_string)
        .ok_or(error::Error::parse_error("failed to find author"))?;

    let short_description = el
        .select(&selectors::WORKSHOP_SHORT_DESC)
        .next()
        .and_then(|s| s.text().next())
        .map(str::trim)
        .map(str::to_string);

    let preview_url = el
        .select(&selectors::ITEM_PREVIEW_IMAGE)
        .next()
        .and_then(|p| p.attr("src"))
        .map(str::to_string);

    let stars = parse_score_small(el);

    Ok(WorkshopCollectionItem {
        id,
        title,
        author_name,
        short_description,
        preview_url,
        stars,
    })
}

fn parse_collection_browse(doc: scraper::Html) -> Result<QueryCollectionResult, error::Error> {
    let pages = parse_browse_pages(&doc)?;

    let items = doc
        .select(&selectors::WORKSHOP_ITEM_COLLECTION)
        .map(parse_browse_collection_item)
        .collect::<Result<Arc<[_]>, _>>()?;

    Ok(QueryCollectionResult { pages, items })
}

fn parse_browse_collection_item(el: ElementRef<'_>) -> Result<QueryCollection, error::Error> {
    let id = el
        .attr("data-publishedfileid")
        .ok_or(error::Error::parse_error("collection item missing id"))
        .and_then(|s| {
            s.parse()
                .map_err(|_| error::Error::parse_error("failed to parse collection id"))
        })?;

    let stars = parse_score_small(el);

    let title = el
        .select(&selectors::ITEM_TITLE)
        .next()
        .and_then(|t| t.text().next())
        .map(str::to_string)
        .ok_or(error::Error::parse_error(
            "failed to find collection item title",
        ))?;

    let assembler_name = el
        .select(&selectors::WORKSHOP_AUTHOR_NAME)
        .next()
        .and_then(|a| a.text().next())
        .map(str::to_string)
        .ok_or(error::Error::parse_error(
            "failed to find collection assembler name",
        ))?;

    let preview_url = el
        .select(&selectors::ITEM_PREVIEW_IMAGE)
        .next()
        .and_then(|i| i.attr("src"))
        .map(str::to_string)
        .ok_or(error::Error::parse_error(
            "failed to get preview image for collection item",
        ))?;

    let short_description = el
        .select(&selectors::WORKSHOP_SHORT_DESC)
        .next()
        .and_then(|d| d.text().next())
        .map(str::to_string);

    Ok(QueryCollection {
        id,
        stars,
        title,
        assembler_name,
        preview_url,
        short_description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrono_parse_test() -> Result<(), error::Error> {
        println!("{:?}", parse_time("Oct 15, 2025 @ 11:47pm")?);
        println!("{:?}", parse_time("Jan 26 @ 1:57pm")?);
        Ok(())
    }

    struct TestProvider {
        client: reqwest::Client,
    }

    impl TestProvider {
        fn new() -> Self {
            Self {
                client: reqwest::Client::default(),
            }
        }
    }

    impl PageProvider for TestProvider {
        type Error = reqwest::Error;
        async fn request_page<U: reqwest::IntoUrl + Send>(
            &self,
            url: U,
        ) -> Result<String, Self::Error> {
            let req = self.client.get(url).build()?;
            self.client.execute(req).await?.text().await
        }
    }

    #[tokio::test]
    async fn doc_parse_test() -> Result<(), error::Error> {
        let provider = TestProvider::new();

        let details = provider.request_item_details(1134256495).await?;
        assert_eq!(details.creator, "76561198372527645");
        println!("{details:#?}");

        Ok(())
    }

    #[tokio::test]
    async fn browse_parse_test() -> Result<(), error::Error> {
        let provider = TestProvider::new();

        let result = provider
            .query_items(
                268500,
                1,
                QueryParams {
                    search_text: String::from("highlander"),
                    sort_method: QuerySort::Trend(QueryPeriod::AllTime),
                    tags: BTreeSet::new(),
                },
            )
            .await?;
        println!("{result:#?}");

        Ok(())
    }

    #[tokio::test]
    async fn collection_parse_test() -> Result<(), error::Error> {
        const COLLECTION_ID: u64 = 2991497212;
        let provider = TestProvider::new();

        let details = provider.request_collection_details(COLLECTION_ID).await?;
        println!("{details:#?}");

        Ok(())
    }

    #[tokio::test]
    async fn collection_browse_parse_test() -> Result<(), error::Error> {
        let provider = TestProvider::new();
        let result = provider
            .query_collections(
                268500,
                1,
                QueryParams {
                    search_text: String::from(""),
                    sort_method: QuerySort::Trend(QueryPeriod::AllTime),
                    tags: BTreeSet::new(),
                },
            )
            .await?;
        println!("{result:#?}");

        Ok(())
    }
}
