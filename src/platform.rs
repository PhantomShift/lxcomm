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

#[cfg(target_os = "linux")]
pub mod extensions {
    pub trait NotificationExtLinux {
        fn desktop_entry(&mut self, name: &str) -> &mut Self;
        fn auto_desktop_entry(&mut self) -> &mut Self {
            let binary = std::env::current_exe().ok();
            let name = binary
                .as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or_default();

            self.desktop_entry(name)
        }
    }

    impl NotificationExtLinux for notify_rust::Notification {
        fn desktop_entry(&mut self, name: &str) -> &mut Self {
            self.hint(notify_rust::Hint::DesktopEntry(name.to_owned()))
        }
    }
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
