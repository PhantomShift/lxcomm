use apply::Apply;
use derivative::Derivative;
use iced::widget::{
    button, checkbox, column, container, pane_grid, pick_list, row, space, text, text_input,
    toggler,
};
use iced_aw::widget::labeled_frame::LabeledFrame;

use crate::{App, AsyncDialogKey, Message};

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
    StringEnum {
        current: Option<String>,
        options: Vec<String>,
    },
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

    pub fn get_string_enum<S: AsRef<str>>(&self, name: S) -> Option<&str> {
        match self.0.get(name.as_ref()) {
            Some(AsyncDialogField::StringEnum { current, .. }) => current.as_deref(),
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
                            container(checkbox(*b).on_toggle(on_update!(name, Checkbox)))
                        }
                        AsyncDialogField::StringEnum { current, options } => container(pick_list(
                            options.as_slice(),
                            current.as_ref(),
                            move |new| {
                                Message::AsyncDialogUpdate(
                                    self.key.clone(),
                                    name.clone(),
                                    AsyncDialogField::StringEnum {
                                        current: Some(new),
                                        options: options.clone(),
                                    },
                                )
                            },
                        )),
                    },
                )
                .into()
            })),
        )
        .foot(row![
            button("Cancel")
                .on_press_with(|| Message::AsyncDialogResolved(self.key.clone(), false)),
            space::horizontal(),
            button("Submit").on_press_with(|| Message::AsyncDialogResolved(self.key.clone(), true))
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

    pub fn with_string_enum<S: Into<String>, I: IntoIterator<Item = String>>(
        self,
        name: S,
        items: I,
    ) -> Self {
        self.with_string_enum_default(name, items, None)
    }

    pub fn with_string_enum_default<S: Into<String>, I: IntoIterator<Item = String>>(
        mut self,
        name: S,
        items: I,
        default: Option<String>,
    ) -> Self {
        self.inner.fields.0.insert(
            name.into(),
            AsyncDialogField::StringEnum {
                current: default,
                options: Vec::from_iter(items),
            },
        );
        self
    }
}

enum ProfilePane {
    ProfileList,
    ModList,
    ModEditor,
}

pub struct ProfilePaneState {
    inner: iced::widget::pane_grid::State<ProfilePane>,
}

impl AsRef<iced::widget::pane_grid::State<ProfilePane>> for ProfilePaneState {
    fn as_ref(&self) -> &iced::widget::pane_grid::State<ProfilePane> {
        &self.inner
    }
}

impl AsMut<iced::widget::pane_grid::State<ProfilePane>> for ProfilePaneState {
    fn as_mut(&mut self) -> &mut iced::widget::pane_grid::State<ProfilePane> {
        &mut self.inner
    }
}

impl Default for ProfilePaneState {
    fn default() -> Self {
        use iced::widget::pane_grid;
        Self {
            inner: pane_grid::State::with_configuration(pane_grid::Configuration::Split {
                axis: pane_grid::Axis::Vertical,
                ratio: 0.2,
                a: Box::new(pane_grid::Configuration::Pane(ProfilePane::ProfileList)),
                b: Box::new(pane_grid::Configuration::Split {
                    axis: pane_grid::Axis::Horizontal,
                    ratio: 0.3,
                    a: Box::new(pane_grid::Configuration::Pane(ProfilePane::ModList)),
                    b: Box::new(pane_grid::Configuration::Pane(ProfilePane::ModEditor)),
                }),
            }),
        }
    }
}

impl App {
    pub fn handle_profile_resize(&mut self, resize: pane_grid::ResizeEvent) {
        self.profile_pane_state
            .as_mut()
            .resize(resize.split, resize.ratio);
    }

    pub fn profiles_page(&self) -> iced::Element<'_, Message> {
        use iced::Fill;
        use iced::widget::pane_grid;

        let profile = self
            .selected_profile_name
            .as_ref()
            .and_then(|name| self.profiles.get(name.as_ref()));

        pane_grid(
            self.profile_pane_state.as_ref(),
            move |_pane, state, _is_maximized| {
                match state {
                    ProfilePane::ProfileList => {
                        macro_rules! sel_button {
                            ($inner:expr) => {
                                button(text($inner).height(Fill).width(Fill).size(14)).height(30)
                            };
                        }

                        let select_col = column(self.profiles.values().map(|profile| {
                            let style = if self
                                .selected_profile_name
                                .as_ref()
                                .is_some_and(|name| **name == *profile.name)
                            {
                                button::secondary
                            } else {
                                button::primary
                            };

                            sel_button!(profile.name.as_str())
                                .style(style)
                                .on_press_with(|| Message::ProfileSelected(profile.name()))
                                .into()
                        }))
                        .push(sel_button!("Add Profile +").on_press(Message::ProfileAddPressed))
                        .push(
                            sel_button!("Import Snapshot +")
                                .on_press(Message::ProfileImportSnapshotRequested),
                        );
                        container(select_col)
                    }
                    ProfilePane::ModList => {
                        container(profile.map(|profile| self.view_profile_mod_list(profile)))
                    }
                    ProfilePane::ModEditor => container(
                        profile
                            .and_then(|profile| {
                                Some((profile, profile.view_selected_item.as_ref()?))
                            })
                            .map(|(profile, item_id)| {
                                self.view_profile_mod_editor(profile, item_id)
                            }),
                    ),
                }
                .padding(4)
                .apply(pane_grid::Content::new)
                .style(|theme| {
                    // To make it more obvious that you can resize
                    let mut style = iced::widget::container::bordered_box(theme);
                    style.border.color = theme.extended_palette().secondary.strong.color;
                    style.border.width *= 2.0;
                    style
                })
            },
        )
        .on_resize(8, Message::ProfilePageResized)
        .into()
    }
}
