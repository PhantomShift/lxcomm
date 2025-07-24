use derivative::Derivative;
use iced::widget::{
    button, checkbox, column, container, horizontal_space, row, text, text_input, toggler,
};
use iced_aw::widget::labeled_frame::LabeledFrame;

use crate::{AsyncDialogKey, Message};

#[macro_export]
macro_rules! tooltip {
    ($content:expr, $tooltip:expr$(,)*) => {
        tooltip!($content, $tooltip, tooltip::Position::FollowCursor)
    };
    ($content:expr, $tooltip:expr, $position:expr $(,)*) => {
        iced::widget::tooltip(
            $content,
            iced::widget::container($tooltip)
                .padding(16)
                .style(container::rounded_box),
            $position,
        )
    };
}

#[derive(Debug, Clone, PartialEq)]
pub enum AsyncDialogField {
    String(String),
    Number(i64),
    Toggle(bool),
    Checkbox(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AsyncDialogFields(pub ringmap::RingMap<String, AsyncDialogField>);

macro_rules! get_field {
    ($method:ident, $field:ident, $type:ty) => {
        pub fn $method<S: AsRef<str>>(&self, name: S) -> Option<$type> {
            match self.0.get(name.as_ref()) {
                Some(AsyncDialogField::$field(val)) => Some(val),
                _ => None,
            }
        }
    };
}

impl AsyncDialogFields {
    get_field!(get_string, String, &str);
    get_field!(get_number, Number, &i64);

    pub fn get_bool<S: AsRef<str>>(&self, name: S) -> Option<bool> {
        match self.0.get(name.as_ref()) {
            Some(AsyncDialogField::Toggle(b) | AsyncDialogField::Checkbox(b)) => Some(*b),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Derivative)]
#[derivative(PartialEq)]
pub struct AsyncDialog {
    pub fields: AsyncDialogFields,
    pub title: String,
    pub body: String,
    pub key: AsyncDialogKey,
    #[derivative(PartialEq = "ignore")]
    pub sender: iced::futures::channel::mpsc::Sender<Option<AsyncDialogFields>>,
}

impl AsyncDialog {
    pub fn builder<T: Into<String>, B: Into<String>>(
        key: AsyncDialogKey,
        title: T,
        body: B,
    ) -> AsyncDialogBuilder {
        AsyncDialogBuilder::new(key, title, body)
    }

    pub fn view(&self) -> iced_aw::Card<'_, Message> {
        macro_rules! on_update {
            ($name:expr, $field:ident) => {
                move |v| {
                    Message::AsyncDialogUpdate(
                        self.key.clone(),
                        $name.clone(),
                        AsyncDialogField::$field(v),
                    )
                }
            };
        }

        iced_aw::card(
            text(&self.title),
            column![text(&self.body)].extend(self.fields.0.iter().map(|(name, field)| {
                LabeledFrame::new(
                    text(name),
                    match field {
                        AsyncDialogField::String(s) => {
                            container(text_input(name, s).on_input(on_update!(name, String)))
                        }
                        AsyncDialogField::Number(p) => container(
                            iced_aw::number_input(p, i64::MIN..i64::MAX, |_| Message::None)
                                .on_input(on_update!(name, Number)),
                        ),
                        AsyncDialogField::Toggle(b) => {
                            container(toggler(*b).on_toggle(on_update!(name, Toggle)))
                        }
                        AsyncDialogField::Checkbox(b) => {
                            container(checkbox("", *b).on_toggle(on_update!(name, Checkbox)))
                        }
                    },
                )
                .into()
            })),
        )
        .foot(row![
            button("Cancel").on_press(Message::AsyncDialogResolved(self.key, false)),
            horizontal_space(),
            button("Submit").on_press(Message::AsyncDialogResolved(self.key, true))
        ])
    }
}

pub struct AsyncDialogBuilder {
    inner: AsyncDialog,
    receiver: iced::futures::channel::mpsc::Receiver<Option<AsyncDialogFields>>,
}

macro_rules! add_with {
    ($method:ident, $field:ident, $type:ty) => {
        pub fn $method<S: Into<String>>(mut self, name: S, default: $type) -> Self {
            self.inner
                .fields
                .0
                .insert(name.into(), AsyncDialogField::$field(default.into()));
            self
        }
    };

    ($method:ident, $default:expr) => {
        pub fn $method<S: Into<String>>(mut self, name: S) -> Self {
            self.inner.fields.0.insert(name.into(), $default);
            self
        }
    };
}

impl AsyncDialogBuilder {
    pub fn new<T: Into<String>, B: Into<String>>(key: AsyncDialogKey, title: T, body: B) -> Self {
        let (sender, receiver) = iced::futures::channel::mpsc::channel(1);
        Self {
            inner: AsyncDialog {
                fields: AsyncDialogFields(ringmap::RingMap::new()),
                title: title.into(),
                body: body.into(),
                key,
                sender,
            },
            receiver,
        }
    }

    pub fn finish(
        self,
    ) -> (
        AsyncDialog,
        iced::futures::channel::mpsc::Receiver<Option<AsyncDialogFields>>,
    ) {
        let AsyncDialogBuilder { inner, receiver } = self;
        (inner, receiver)
    }

    add_with!(with_string, AsyncDialogField::String(String::new()));
    add_with!(with_number, AsyncDialogField::Number(0));
    add_with!(with_toggler, AsyncDialogField::Toggle(false));
    add_with!(with_checkbox, AsyncDialogField::Checkbox(false));

    add_with!(with_string_default, String, impl Into<String>);
    add_with!(with_number_default, Number, impl Into<i64>);
    add_with!(with_toggler_default, Toggle, bool);
    add_with!(with_checkbox_default, Checkbox, bool);
}
