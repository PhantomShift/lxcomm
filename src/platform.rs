#[cfg(not(target_os = "windows"))]
pub mod symbols {
    pub use fontawesome::check;
    pub use fontawesome::eye;
    pub use fontawesome::folder;
    // Normal magnifying glass doesn't seem to render properly
    pub use fontawesome::magnifying_glass_plus as magnifying_glass;
    pub use fontawesome::xmark;
    use iced_fonts::fontawesome;
}
#[cfg(target_os = "windows")]
pub mod symbols {
    use iced::widget::text;
    use iced::widget::text::Text;

    macro_rules! add_symbol {
        ($name:ident, $symbol:expr) => {
            pub fn $name<'a>() -> Text<'a> {
                text($symbol).shaping(text::Shaping::Advanced)
            }
        };
    }

    add_symbol!(check, "✔️");
    add_symbol!(xmark, "❌");
    add_symbol!(folder, "📂");
    add_symbol!(eye, "👁️");
    add_symbol!(magnifying_glass, "🔎");
}
