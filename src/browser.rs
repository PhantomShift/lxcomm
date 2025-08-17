use std::ops::Not;

use iced::{
    Alignment::Center,
    Element,
    Length::Fill,
    widget::{
        button, column, container, grid, horizontal_space, pick_list, row, scrollable, text,
        text_input, toggler,
    },
};
use secrecy::ExposeSecret;
use strum::VariantArray;

use crate::{
    App, Message,
    extensions::DetailsExtension,
    web::{self, WorkshopQuery},
};

#[macro_export]
macro_rules! reset_scroll {
    ($id:expr) => {{
        let op = iced::advanced::widget::operation::scrollable::scroll_to(
            $id.into(),
            iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: 0.0 },
        );
        iced::advanced::widget::operate(op)
    }};
}

// Potential TODO - Associated message enum only for browse changes
// which must be consumed per-implementor
pub trait WorkshopBrowser {
    type Item;
    type Message: Into<Message>;

    fn get_page(&self) -> u32;
    fn get_max_page(&self) -> u32;
    fn get_query(&self) -> &WorkshopQuery;
    fn get_tags_toggled(&self) -> bool;
    fn get_scroll_id(&self) -> iced::widget::scrollable::Id;

    fn make_grid(&self) -> iced::widget::Grid<'_, Message> {
        grid::Grid::new()
            .spacing(16)
            .fluid(360)
            .height(grid::Sizing::AspectRatio(0.5))
    }

    /// Strategy for showing the item in a small box
    fn render_preview_box<'a>(
        &'a self,
        state: &'a App,
        item: &'a Self::Item,
    ) -> Element<'a, Message>;

    fn render_browser<'a>(
        &'a self,
        state: &'a App,
        items: impl IntoIterator<Item = &'a Self::Item>,
    ) -> Element<'a, Message> {
        let current_page = self.get_page();
        let max_page = self.get_max_page();
        let query = self.get_query();

        let rendered_items = self.make_grid().extend(items.into_iter().map(|item| {
            container(self.render_preview_box(state, item))
                .style(container::secondary)
                .padding(16)
                .into()
        }));

        column![
            row![
                text("Search"),
                text_input("Query...", &query.query)
                    .on_input(|s| self.on_query_text_edited(state, s).into())
                    .on_submit(self.on_query_submitted(state).into())
            ],
            row![pick_list(
                web::WorkshopSort::all_with_period(query.sort.period_or_default()),
                Some(&query.sort),
                |new| self.on_query_sort_edited(state, new).into()
            )]
            .push(match &query.sort {
                web::WorkshopSort::Trend(period) => {
                    Some(pick_list(
                        web::WorkshopTrendPeriod::VARIANTS,
                        Some(period),
                        |new| self.on_query_period_edited(state, new).into(),
                    ))
                }
                _ => None,
            })
            .push(
                iced_aw::DropDown::new(
                    button("Tags").width(256).style(button::secondary).on_press(
                        self.on_query_tags_toggled(state, !self.get_tags_toggled())
                            .into()
                    ),
                    container(column(web::XCOM2WorkshopTag::VARIANTS.iter().map(|tag| {
                        row![
                            text(tag.to_string()),
                            horizontal_space(),
                            toggler(query.tags.contains(tag))
                                .on_toggle(|_| self.on_query_tag_edited(state, *tag).into())
                        ]
                        .into()
                    })),)
                    .style(container::dark)
                    .padding(8),
                    self.get_tags_toggled(),
                )
                .on_dismiss(self.on_query_tags_toggled(state, false).into())
            ),
            scrollable(container(rendered_items).padding(16))
                .height(Fill)
                .anchor_top()
                .id(self.get_scroll_id()),
            row![
                button("<").on_press_maybe(
                    (max_page > 0 && current_page > 1)
                        .then(|| self.on_page_change(state, current_page - 1).into())
                ),
                horizontal_space(),
                container(if max_page > 0 {
                    text!("Page {current_page} of {max_page}")
                } else {
                    text!("Nothing to see...")
                })
                .padding(4),
                horizontal_space(),
                button(">").on_press_maybe(
                    (max_page > 0 && current_page < max_page)
                        .then(|| self.on_page_change(state, current_page + 1).into())
                )
            ]
        ]
        .into()
    }

    fn on_page_change(&self, state: &App, new: u32) -> Self::Message;
    fn on_query_text_edited(&self, state: &App, new: String) -> Self::Message;
    fn on_query_sort_edited(&self, state: &App, new: web::WorkshopSort) -> Self::Message;
    fn on_query_period_edited(&self, state: &App, new: web::WorkshopTrendPeriod) -> Self::Message;
    fn on_query_submitted(&self, state: &App) -> Self::Message;
    fn on_query_tags_toggled(&self, state: &App, toggled: bool) -> Self::Message;
    fn on_query_tag_edited(&self, state: &App, tag: web::XCOM2WorkshopTag) -> Self::Message;
}

impl WorkshopBrowser for App {
    type Item = steam_rs::published_file_service::query_files::File;
    type Message = Message;

    fn get_max_page(&self) -> u32 {
        self.browsing_page_max
    }
    fn get_page(&self) -> u32 {
        self.browsing_page
    }
    fn get_query(&self) -> &WorkshopQuery {
        &self.browsing_query
    }
    fn get_tags_toggled(&self) -> bool {
        self.browse_query_tags_open
    }
    fn get_scroll_id(&self) -> iced::widget::scrollable::Id {
        self.browsing_scroll_id.clone()
    }
    fn on_page_change(&self, _state: &App, new: u32) -> Self::Message {
        Message::SetBrowsePage(new)
    }
    fn on_query_submitted(&self, _state: &App) -> Self::Message {
        Message::BrowseSubmitQuery
    }
    fn on_query_text_edited(&self, _state: &App, new: String) -> Self::Message {
        Message::BrowseEditQuery(new)
    }
    fn on_query_sort_edited(&self, _state: &App, new: web::WorkshopSort) -> Self::Message {
        Message::BrowseEditSort(new)
    }
    fn on_query_period_edited(&self, _state: &App, new: web::WorkshopTrendPeriod) -> Self::Message {
        Message::BrowseEditPeriod(new)
    }
    fn on_query_tags_toggled(&self, _state: &App, toggled: bool) -> Self::Message {
        Message::BrowseToggleTagsDropdown(toggled)
    }
    fn on_query_tag_edited(&self, _state: &App, tag: web::XCOM2WorkshopTag) -> Self::Message {
        Message::BrowseEditTag(tag)
    }
    fn render_preview_box<'a>(
        &'a self,
        _state: &'a App,
        file: &'a Self::Item,
    ) -> Element<'a, Message> {
        let id = &file
            .published_file_id
            .parse::<u32>()
            .expect("id should be a valid u32");
        column![
            web::image_maybe(&self.images, &file.preview_url)
                .width(Fill)
                .height(300),
            text(&file.title),
            text(&file.published_file_id),
            text(web::get_user_display_name(
                steam_rs::Steam::new(self.api_key.expose_secret()),
                file.creator.0
            )),
            text!("{:.2} out of 10", file.get_score() * 10.0),
            button(text("View").align_x(Center))
                .width(Fill)
                .on_press(Message::SetViewingItem(id.into())),
            button(
                text(if self.item_downloaded(*id) {
                    "Update"
                } else {
                    "Download"
                })
                .align_x(Center)
            )
            .width(Fill)
            .on_press_maybe(
                self.is_downloading(*id)
                    .not()
                    .then_some(Message::SteamCMDDownloadRequested(*id))
            ),
            text(file.get_description()).shaping(text::Shaping::Advanced),
        ]
        .clip(true)
        .width(Fill)
        .into()
    }
}
