use std::{
    collections::BTreeSet,
    sync::{Arc, LazyLock},
};

use chrono::{Datelike, NaiveDateTime};
use strum::VariantArray;

#[macro_use]
extern crate markup5ever;

pub mod descriptions;

mod selectors {
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

    // Workshop browse page selectors
    selector!(BROWSE_ITEMS, ".workshopBrowseItems");
    selector!(AUTHOR_LINK, ".workshop_author_link");
    selector!(BROWSE_PREVIEW_IMAGE, ".workshopItemPreviewImage ");
    selector!(FILE_RATING, ".fileRating");
    selector!(UGC, ".ugc");
    selector!(NO_ITEMS, "#no_items");
    selector!(PAGING_INFO, ".workshopBrowsePagingInfo");
}

pub mod error {
    #[derive(Debug)]
    pub enum ErrorKind {
        ChronoParseError(chrono::ParseError),
        RequestError(Box<dyn std::error::Error>),
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

#[derive(Debug)]
pub struct QueryItem {
    pub id: u64,
    pub stars: u8,
    pub title: String,
    pub author_name: String,
    pub author_id: String,
    pub preview_url: String,
    pub short_description: Option<String>,
}

#[derive(Debug)]
pub struct QueryResult {
    pub pages: u32,
    pub items: Arc<[QueryItem]>,
}

pub trait PageProvider {
    type Error: std::error::Error + 'static;

    fn build_item_url(id: u64) -> String {
        format!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}&l=english")
    }

    fn build_browse_url(app_id: u64, page: u32, params: QueryParams) -> String {
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
        base.to_string()
    }

    fn parse_item(page: &str) -> Result<WorkshopFile, error::Error> {
        parse_document(scraper::Html::parse_document(page))
    }

    fn parse_browse(page: &str) -> Result<QueryResult, error::Error> {
        let page = scraper::Html::parse_document(page);
        parse_browse_result(page)
    }

    fn request_page(
        &self,
        url: String,
    ) -> impl std::future::Future<Output = Result<String, Self::Error>> + Send;

    fn request_page_wrapped(
        &self,
        url: String,
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

            Self::parse_item(&page).map(Arc::new)
        }
    }

    fn query_items(
        &self,
        app_id: u64,
        page: u32,
        params: QueryParams,
    ) -> impl std::future::Future<Output = Result<Arc<QueryResult>, error::Error>> {
        async move {
            let url = Self::build_browse_url(app_id, page, params);
            let page = self.request_page_wrapped(url).await?;
            Self::parse_browse(&page).map(Arc::new)
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

    let title = doc
        .select(&selectors::ITEM_TITLE)
        .next()
        .ok_or_else(|| error::Error::parse_error("Missing workshop item title"))?
        .inner_html();

    let file_description = doc
        .select(&selectors::DESCRIPTION)
        .next()
        .map(|el| el.inner_html());

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

    let tags = doc
        .select(&selectors::RIGHT_DETAILS_BLOCK)
        .next()
        .map(|e| e.child_elements())
        .into_iter()
        .flatten()
        .filter_map(|el| {
            let s = el.inner_html();
            if !s.is_empty() { Some(s) } else { None }
        })
        .collect();

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
    })
}

fn parse_browse_result(doc: scraper::Html) -> Result<QueryResult, error::Error> {
    if doc.select(&selectors::NO_ITEMS).next().is_some() {
        return Ok(QueryResult {
            pages: 0,
            items: Arc::new([]),
        });
    }

    let paging_info = doc
        .select(&selectors::PAGING_INFO)
        .next()
        .ok_or(error::Error::parse_error("failed to get paging info"))?
        .inner_html();

    let pages = paging_info
        .split_once(" of ")
        .ok_or(error::Error::parse_error("failed to get paging info"))?
        .1
        .trim_end_matches(" entries")
        .parse::<u32>()
        .map_err(|_| error::Error::parse_error("failed to get paging info"))?
        / 30
        + 1;

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

        // eugh but idk a more convenient method
        let short_description = script
            .inner_html()
            .split_once(r#"description":""#)
            .and_then(|split| split.1.rsplit_once(r#"","user_subscribed"#))
            .map(|split| split.0.to_string());

        items.push(QueryItem {
            id: item
                .select(&selectors::UGC)
                .next()
                .and_then(|ugc| ugc.attr("data-publishedfileid"))
                .ok_or(error::Error::parse_error("item missing id"))?
                .parse::<u64>()
                .map_err(|_| error::Error::parse_error("invalid id"))?,
            stars: item
                .select(&selectors::FILE_RATING)
                .next()
                .and_then(|rating| rating.attr("src"))
                .and_then(|src| reqwest::Url::parse(src).ok())
                .and_then(|url| {
                    url.path_segments().into_iter().flatten().last().map(
                        |file_name| match file_name {
                            "5-star.png" => 5,
                            "4-star.png" => 4,
                            "3-star.png" => 3,
                            "2-star.png" => 2,
                            "1-star.png" => 1,
                            _ => 0,
                        },
                    )
                })
                .unwrap_or(0),
            title: item
                .select(&selectors::ITEM_TITLE)
                .next()
                .ok_or(error::Error::parse_error("workshop item missing title"))?
                .inner_html(),
            author_name: author_link.inner_html(),
            author_id,
            short_description,
            preview_url: item
                .select(&selectors::BROWSE_PREVIEW_IMAGE)
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

    Ok(QueryResult {
        pages,
        items: items.into(),
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

    impl PageProvider for TestProvider {
        type Error = reqwest::Error;
        async fn request_page(&self, url: String) -> Result<String, Self::Error> {
            let req = self.client.get(url).build()?;
            self.client.execute(req).await?.text().await
        }
    }

    #[tokio::test]
    async fn doc_parse_test() -> Result<(), error::Error> {
        let provider = TestProvider {
            client: reqwest::Client::default(),
        };

        let details = provider.request_item_details(1134256495).await?;
        assert_eq!(details.creator, "76561198372527645");
        println!("{details:#?}");

        Ok(())
    }

    #[tokio::test]
    async fn browse_parse_test() -> Result<(), error::Error> {
        let provider = TestProvider {
            client: reqwest::Client::default(),
        };

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
}
