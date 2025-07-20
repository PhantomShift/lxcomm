use steam_rs;

pub trait DetailsExtension {
    fn get_description(&self) -> &str;
    fn get_score(&self) -> f32;
}

impl DetailsExtension for steam_rs::published_file_service::query_files::File {
    fn get_description(&self) -> &str {
        self.file_description
            .as_ref()
            .or(self.short_description.as_ref())
            .map_or("", |desc| desc.as_str())
    }

    fn get_score(&self) -> f32 {
        self.vote_data.as_ref().map_or(0.0, |data| data.score)
    }
}

pub trait SplitAtBoundary {
    fn split_at_boundary(&self, predicate: fn(char, char) -> bool) -> impl Iterator<Item = &str>;
    fn split_at_case(&self) -> impl Iterator<Item = &str> {
        self.split_at_boundary(|l, r| l.is_lowercase() && r.is_uppercase())
    }
}

impl<S> SplitAtBoundary for S
where
    S: AsRef<str>,
{
    fn split_at_boundary(&self, predicate: fn(char, char) -> bool) -> impl Iterator<Item = &str> {
        let mut last_index = 0;
        let mut indices = self
            .as_ref()
            .char_indices()
            .map_windows(move |[(_li, lc), (ri, rc)]| (*ri, predicate(*lc, *rc)));

        std::iter::from_fn(move || {
            if last_index >= self.as_ref().len() {
                return None;
            }

            for (i, should_split) in indices.by_ref() {
                if should_split {
                    let slice = &self.as_ref()[last_index..i];
                    last_index = i;
                    return Some(slice);
                }
            }
            let remaining = &self.as_ref()[last_index..];
            last_index = self.as_ref().len();
            Some(remaining)
        })
    }
}

// Temporary solution until upstream fix is accepted
// https://github.com/garhow/steam-rs/pull/57
pub trait URLArrayListable: IntoIterator {
    type Item: ToString;
    fn into_url_array(self, name: &str) -> String;
}

impl<T> URLArrayListable for T
where
    T: IntoIterator,
    <T as std::iter::IntoIterator>::Item: ToString,
{
    type Item = T::Item;

    // https://steamcommunity.com/workshop/browse/?appid=268500&searchtext=hololive+test&childpublishedfileid=0&browsesort=trend&section=readytouseitems&requiredtags%5B%5D=War+of+the+Chosen&created_date_range_filter_start=0&created_date_range_filter_end=0&updated_date_range_filter_start=0&updated_date_range_filter_end=0

    fn into_url_array(self, name: &str) -> String {
        let c = self
            .into_iter()
            .enumerate()
            .map(|(i, item)| format!("{name}[{i}]={}", item.to_string()))
            .collect::<Vec<_>>();

        c.join("&")
    }
}

// TODO - Use notify-rust to allow notifications on windows as well
// https://github.com/hoodie/notify-rust
#[cfg(target_os = "linux")]
pub mod zbus_ext {
    use serde::Serialize;
    use zbus::zvariant::SerializeDict;

    #[derive(Debug, Default, Serialize, zbus::zvariant::Type)]
    pub struct NotificationParameters {
        pub app_name: String,
        pub replaces_id: u32,
        pub app_icon: String,
        pub summary: String,
        pub body: String,
        pub actions: Vec<String>,
        pub hints: NotificationHint,
        pub expire_timeout: i32,
    }

    #[derive(Debug, Default, strum::Display, zbus::zvariant::Type)]
    #[zvariant(signature = "s")]
    pub enum NotificationCategory {
        Call,
        CallEnded,
        CallIncoming,
        Device,
        DeviceAdded,
        DeviceError,
        DeviceRemoved,
        Email,
        EmailArrived,
        EmailBounced,
        #[strum(serialize = "Im")]
        InstantMessage,
        #[strum(serialize = "ImError")]
        InstantMessageError,
        #[strum(serialize = "ImReceived")]
        InstantMessageReceived,
        Network,
        NetworkConnected,
        NetworkError,
        Presence,
        PresenceOffline,
        PresenceOnline,
        Transfer,
        TransferComplete,
        TransferError,

        #[default]
        None,
    }

    impl Serialize for NotificationCategory {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            if let Self::None = self {
                return serializer.serialize_str("");
            }

            let s = self.to_string();
            if let Some((i, _ch)) = s
                .char_indices()
                .skip(1)
                .find(|(_i, ch)| ch.is_ascii_uppercase())
            {
                let (l, r) = s.split_at(i);
                serializer.serialize_str(&format!("{}.{}", l.to_lowercase(), r.to_lowercase()))
            } else {
                serializer.serialize_str(&s.to_lowercase())
            }
        }
    }

    #[test]
    fn notif_serialize_test() {
        assert_eq!(
            "[\"im.error\"]",
            serde_json::to_string(&[NotificationCategory::InstantMessageError]).unwrap()
        )
    }

    #[derive(Debug, Default, SerializeDict, zbus::zvariant::Type)]
    #[zvariant(signature = "dict", rename_all = "kebab-case")]
    pub struct NotificationHint {
        /// If enabled, attempts to interpret any action identifier
        /// as a named icon in actions.
        pub action_icons: bool,
        /// The type of notification
        pub category: NotificationCategory,
        /// Name of the desktop filename from which the notification originates.
        /// The prefix of the applications `.desktop` file.
        pub desktop_entry: String,
        /// Alternative method of defining notification image.
        pub image_path: String,
        /// If set and the server has "persistence" capability,
        /// the notification will not be removed until it is removed
        /// by the user or the sender.
        pub resident: bool,
        /// Path to the sound file to play when notification pops up.
        pub sound_file: String,
        /// Themeable sound to play based on [freedesktop naming specification](http://0pointer.de/public/sound-naming-spec.html).
        pub sound_name: String,
        /// If enabled and server has the "sound" capability,
        /// causes server to suppress playing any sounds.
        pub suppress_sound: bool,
        /// If enabled, sets notification to not be recorded by servers that respect this.
        pub transient: bool,
        /// Specifies requested x-location on the screen; `y` must also be specified.
        pub x: Option<i32>,
        /// Specifies requested y-location on the screen; `x` must also be specified.
        pub y: Option<i32>,
        /// Urgency level as a byte value.
        /// * `0` - Low
        /// * `1` - Normal
        /// * `2` - Critical
        pub urgency: u8,
    }

    pub trait NotificationSender {
        type Return;
        type Error;

        // Only expected to be used in this application
        #[allow(async_fn_in_trait)]
        async fn send_notification(
            &self,
            params: NotificationParameters,
        ) -> Result<Self::Return, Self::Error>;
    }

    impl NotificationSender for zbus::Connection {
        type Return = zbus::Message;
        type Error = zbus::Error;

        async fn send_notification(
            &self,
            params: NotificationParameters,
        ) -> Result<zbus::Message, zbus::Error> {
            self.call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "Notify",
                &params,
            )
            .await
        }
    }

    #[tokio::test]
    async fn notification_test() {
        let connection = zbus::Connection::session().await.unwrap();
        let params = NotificationParameters {
            app_name: "LXCOMM Test".to_string(),
            summary: "Test Notification".to_string(),
            body: "This is a test notification".to_string(),
            hints: NotificationHint {
                sound_name: "complete-download".to_string(),
                ..Default::default()
            },
            expire_timeout: -1,
            ..Default::default()
        };

        connection.send_notification(params).await.unwrap();
    }
}
