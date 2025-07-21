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
