use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use crate::{CACHE_DIR, Message};
use eyre::Result;
use iced::{
    Length::Fill,
    futures::{SinkExt, Stream, StreamExt},
    widget::{Container, container, image},
};
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
                match crate::get_mod_details(current_client.clone(), &unresolved).await {
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
                            eprintln!("The API key the background resolver was sent does not appear to be valid.");
                        }
                    }
                }
            }
        }
    })
}
