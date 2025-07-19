use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use crate::{CACHE_DIR, Message, XCOM_APPID};
use eyre::Result;
use iced::{
    Length::Fill,
    futures::{SinkExt, Stream, StreamExt},
    widget::{Container, container, image},
};
use steam_rs::{
    Steam,
    published_file_service::{get_details::FileDetailResult, query_files::PublishedFiles},
};

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

// TODO - Introduce option for size limit of on-disk cache
static QUERY_CACHE: LazyLock<moka::future::Cache<String, PublishedFiles>> =
    LazyLock::new(|| moka::future::Cache::new(64));
pub const QUERY_PAGE_SIZE: u32 = 30;
pub async fn query_mods<S: Into<String>>(
    client: Steam,
    page: u32,
    query: S,
    cache_lifetime: u32,
) -> Result<(PublishedFiles, bool)> {
    let query: String = query.into();

    let cache_key = format!("{page}|{query}");

    if let Some(file) = QUERY_CACHE.get(&cache_key).await {
        return Ok((file, true));
    }

    let clean = query.replace(' ', "_").to_ascii_lowercase();

    // TODO - Different expose ranking options
    // let cached = CACHED_QUERIES.join(format!("{clean}_update_page_{page}.json"));
    let cached = crate::CACHED_QUERIES.join(format!("{clean}_relevance_page_{page}.json"));
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
            // steam_rs::published_file_service::query_files::PublishedFileQueryType::RankedByLastUpdatedDate,
            steam_rs::published_file_service::query_files::PublishedFileQueryType::RankedByTrend,
            page,
            "",
            Some(QUERY_PAGE_SIZE),
            XCOM_APPID,
            XCOM_APPID,
            "",
            "",
            None,
            "",
            "",
            &query,
            steam_rs::published_file_service::query_files::PublishedFileInfoMatchingFileType::Items,
            0,
            0,
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
