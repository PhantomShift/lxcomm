use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use iced::{
    Element, Font,
    Length::Fill,
    Task,
    widget::{
        button, column, container, row, scrollable, text, text_editor, text_input, vertical_rule,
    },
};
use similar::ChangeTag;

use crate::{App, Message, PROFILES_DIR, files};

#[derive(Debug, strum::Display, Clone, Copy)]
pub enum DeleteAction {
    /// Reset state to the original mod config
    Reset,
    /// Remove custom config file
    Delete,
}

#[derive(Debug, Clone, Default)]
pub enum EditorMessage {
    NewConfigEdit(String),
    Select(String),
    Load(usize, u32, PathBuf),
    Delete(String, DeleteAction),
    Save(String),
    SaveAll,
    Create,
    BufferEdit(String, text_editor::Action),
    SetPage(EditorPage),
    #[default]
    None,
}

impl From<EditorMessage> for Message {
    fn from(value: EditorMessage) -> Self {
        Message::ModEditor(value)
    }
}

#[derive(Debug, Clone, Default)]
pub enum EditorPage {
    #[default]
    IniEditor,
    Diff,
}

// TODO - figure out per item stuff
#[derive(Default)]
pub struct Editor {
    pub new_config_name: String,
    pub current_root: Option<PathBuf>,
    pub current_file: Option<String>,
    /// Settings that came as part of the original file
    pub original_buffers: HashMap<String, String>,
    /// Settings created by users
    pub custom_buffers: HashSet<String>,
    /// The currently used setting
    pub saved_buffers: HashMap<String, String>,
    /// The transient state of the settings being edited
    pub edit_buffers: HashMap<String, text_editor::Content>,
    pub page: EditorPage,
}

impl Editor {
    pub fn has_unsaved(&self) -> bool {
        self.edit_buffers.keys().any(|p| !self.is_saved(p))
    }

    pub fn is_saved<S: AsRef<str>>(&self, name: S) -> bool {
        let edit = self.edit_buffers.get(name.as_ref()).map(|c| c.text());
        let trimmed = edit.as_ref().map(|s| s.trim_end());
        let save = self.saved_buffers.get(name.as_ref()).map(|s| s.trim_end());
        let original = self
            .original_buffers
            .get(name.as_ref())
            .map(|s| s.trim_end());

        trimmed == save || trimmed == original
    }

    pub fn get_delete_action<S: AsRef<str>>(&self, name: S) -> DeleteAction {
        if self.custom_buffers.contains(name.as_ref()) {
            DeleteAction::Delete
        } else {
            DeleteAction::Reset
        }
    }

    pub fn update(&mut self, message: EditorMessage) -> Option<Task<Message>> {
        match message {
            EditorMessage::NewConfigEdit(s) => self.new_config_name = s,
            EditorMessage::Select(name) => {
                if self.current_file.as_ref() != Some(&name) {
                    self.current_file = Some(name);
                    self.page = EditorPage::IniEditor;
                }
            }
            EditorMessage::BufferEdit(name, action) => {
                if !self.edit_buffers.contains_key(&name) {
                    eprintln!("Attempt to edit non-existent buffer for file {name}",);
                    return None;
                }

                self.edit_buffers
                    .entry(name)
                    .and_modify(|buffer| buffer.perform(action));
            }
            EditorMessage::Create => {
                self.current_file.as_ref()?;

                let name = self.new_config_name.trim_end_matches(".ini").to_string();
                if let Some(s) = ["/", "\\", ".."].iter().find(|s| name.contains(*s)) {
                    return Some(Task::done(Message::DisplayError(
                        "Invalid Name".to_string(),
                        format!("Names cannot contain '{s}'"),
                    )));
                }

                self.new_config_name = String::new();

                if self.original_buffers.contains_key(&name) || self.custom_buffers.contains(&name)
                {
                    return Some(Task::done(Message::DisplayError(
                        "Already exists".to_string(),
                        format!("Config file with name '{name}' already exists."),
                    )));
                }

                self.custom_buffers.insert(name.clone());
                self.saved_buffers.insert(name.clone(), String::new());
                self.edit_buffers.insert(name, text_editor::Content::new());
            }
            EditorMessage::Delete(name, action) => {
                if let Some(root) = &self.current_root {
                    match action {
                        DeleteAction::Delete => {
                            self.edit_buffers.remove(&name);
                            if let Some(current) = &self.current_file
                                && current == &name
                            {
                                self.current_file = None;
                            }
                        }
                        DeleteAction::Reset => {
                            self.edit_buffers.insert(
                                name.clone(),
                                text_editor::Content::with_text(
                                    self.original_buffers
                                        .get(&name)
                                        .map(String::as_str)
                                        .unwrap_or_default(),
                                ),
                            );
                        }
                    }
                    self.saved_buffers.remove_entry(&name);
                    if let Err(err) = std::fs::remove_file(root.join(&name).with_extension("ini")) {
                        eprintln!("Error removing {name}: {err:?}");
                    }
                }
            }
            EditorMessage::Save(path) => {
                if let Err(err) = self.save_buffer(path) {
                    eprintln!("Error saving buffer: {err:?}");
                }
            }
            EditorMessage::SaveAll => {
                let paths = self.edit_buffers.keys().cloned().collect::<Vec<_>>();
                for path in paths {
                    if let Err(err) = self.save_buffer(path) {
                        eprintln!("Error saving buffer: {err:?}");
                    }
                }
            }
            EditorMessage::Load(profile_id, item_id, download_dir) => {
                self.current_root = None;
                self.current_file = None;
                self.original_buffers.clear();
                self.custom_buffers.clear();
                self.saved_buffers.clear();
                self.edit_buffers.clear();
                self.page = EditorPage::IniEditor;

                let profile_path = PROFILES_DIR
                    .join(profile_id.to_string())
                    .join(item_id.to_string());
                if !profile_path.exists()
                    && let Err(err) = std::fs::create_dir_all(&profile_path)
                {
                    eprintln!("Error creating profile directory: {err:?}");
                }

                let config_path = files::get_item_config_directory(download_dir, item_id);
                if config_path.exists() {
                    let result: Result<(), std::io::Error> = try {
                        let read = std::fs::read_dir(&config_path)?;
                        for entry in read {
                            let entry = entry?;
                            if let Some(name) = entry
                                .file_name()
                                .to_str()
                                .map(|s| s.trim_end_matches(".ini").to_owned())
                                && entry.path().extension().is_some_and(|ext| ext == "ini")
                            {
                                let s = std::fs::read_to_string(entry.path())?;
                                self.original_buffers.insert(name, s.clone());
                            }
                        }
                    };
                    if let Err(err) = result {
                        return Some(Task::done(Message::DisplayError(
                            "Error".to_string(),
                            indoc::formatdoc! {"
                                Something went wrong when trying to load the mod's original config files.
                                LXCOMM might be lacking proper file permissions for some reason.
                                Path: {path}
                                Original error: {err:?}
                                ",
                            path = config_path.display()
                            },
                        )));
                    }
                }

                let result: Result<(), std::io::Error> = try {
                    let read = std::fs::read_dir(&profile_path)?;
                    for entry in read {
                        let entry = entry?;
                        if let Some(name) = entry
                            .file_name()
                            .to_str()
                            .map(|s| s.trim_end_matches(".ini").to_owned())
                            && entry.path().extension().is_some_and(|ext| ext == "ini")
                        {
                            let s = std::fs::read_to_string(entry.path())?;
                            self.saved_buffers.insert(name.clone(), s);
                            if !self.original_buffers.contains_key(&name) {
                                self.custom_buffers.insert(name);
                            }
                        }
                    }
                };
                if let Err(err) = result {
                    return Some(Task::done(Message::DisplayError(
                        "Error".to_string(),
                        indoc::formatdoc! {"
                            Something went wrong when trying to load profile information.
                            LXCOMM might be lacking proper file permissions for some reason.
                            Path: {path}
                            Original error: {err:?}
                        ",
                        path = profile_path.display()
                        },
                    )));
                }

                for path in self
                    .original_buffers
                    .keys()
                    .chain(self.custom_buffers.iter())
                {
                    let buffer = self
                        .saved_buffers
                        .get(path)
                        .or(self.original_buffers.get(path))
                        .map(String::as_str)
                        .unwrap_or_default();

                    self.edit_buffers
                        .insert(path.clone(), text_editor::Content::with_text(buffer));
                }

                self.current_root = Some(profile_path);
            }
            EditorMessage::SetPage(page) => self.page = page,
            EditorMessage::None => (),
        }

        None
    }

    pub fn save_buffer<S: AsRef<str>>(&mut self, name: S) -> Result<(), std::io::Error> {
        macro_rules! not_found {
            () => {
                std::io::Error::new(std::io::ErrorKind::NotFound, "path not found")
            };
        }

        let Some(root) = &self.current_root else {
            return Err(not_found!());
        };

        let edit = self.edit_buffers.get(name.as_ref()).ok_or(not_found!())?;

        let to_save = self
            .saved_buffers
            .entry(name.as_ref().to_owned())
            .and_modify(|buffer| *buffer = edit.text())
            .or_insert_with(|| edit.text());

        let path = root.join(name.as_ref()).with_extension("ini");
        if let Some(original) = self.original_buffers.get(name.as_ref())
            && to_save == original
        {
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        } else {
            std::fs::write(path, to_save)?;
        }

        Ok(())
    }

    pub fn view(&self, state: &App) -> Element<'_, Message> {
        let buttons = self.edit_buffers.keys().map(|name| {
            let style = if let Some(sel) = &self.current_file
                && sel == name
            {
                button::secondary
            } else {
                button::primary
            };
            let marker = if !self.is_saved(name) { " (*)" } else { "" };
            button(text!("{name}.ini{marker}",))
                .style(style)
                .on_press(EditorMessage::Select(name.clone()).into())
                .width(Fill)
                .into()
        });

        let buttons_col = column(buttons).push(
            text_input("+ New Config File", &self.new_config_name)
                .on_input(|s| EditorMessage::NewConfigEdit(s).into())
                .on_submit(EditorMessage::Create.into())
                .width(Fill),
        );

        let editor = self.current_file.as_ref().and_then(|name| {
            self.edit_buffers.get(name).map(|content| {
                let original_buffer = self.original_buffers.get(name);
                let delete = self.get_delete_action(name);
                let can_delete = {
                    match &delete {
                        DeleteAction::Delete => true,
                        DeleteAction::Reset => {
                            if let Some(orig) = self.original_buffers.get(name) {
                                content.text().trim_end() != orig.trim_end()
                            } else {
                                false
                            }
                        }
                    }
                };
                column![
                    row![
                        button(text(delete.to_string()))
                            .style(button::danger)
                            .on_press_maybe(
                                can_delete
                                    .then_some(EditorMessage::Delete(name.clone(), delete).into())
                            ),
                        button("Save")
                            .style(button::success)
                            .on_press(EditorMessage::Save(name.clone()).into()),
                        match self.page {
                            EditorPage::IniEditor => button("View Diff").on_press_maybe(
                                original_buffer
                                    .is_some()
                                    .then_some(EditorMessage::SetPage(EditorPage::Diff).into())
                            ),
                            EditorPage::Diff => button("View INI")
                                .on_press(EditorMessage::SetPage(EditorPage::IniEditor).into()),
                        },
                    ],
                    match self.page {
                        EditorPage::IniEditor => container(
                            text_editor(content)
                                .on_action(
                                    |action| EditorMessage::BufferEdit(name.clone(), action).into()
                                )
                                // Potential TODO: Add settings for theme (including application-level)
                                .highlight("ini", iced::highlighter::Theme::Leet)
                                .font(Font::MONOSPACE)
                                .wrapping(text::Wrapping::WordOrGlyph)
                                .height(Fill)
                        ),
                        EditorPage::Diff => {
                            if let Some(original) = original_buffer {
                                let new = content.text();
                                let diff =
                                    similar::TextDiff::from_lines(original.as_str(), new.as_str());
                                container(scrollable(column(diff.iter_all_changes().map(
                                    |change| {
                                        let style = match change.tag() {
                                            ChangeTag::Equal => container::bordered_box,
                                            ChangeTag::Delete => container::danger,
                                            ChangeTag::Insert => container::success,
                                        };
                                        container(
                                            text(change.to_string())
                                                .font(Font::MONOSPACE)
                                                .wrapping(text::Wrapping::WordOrGlyph),
                                        )
                                        .width(Fill)
                                        .style(style)
                                        .into()
                                    },
                                ))))
                            } else {
                                container("you shouldn't be here...")
                            }
                            .style(container::rounded_box)
                            .padding(4)
                            .width(Fill)
                        }
                    },
                ]
            })
        });

        column![row![scrollable(buttons_col).width(200), vertical_rule(2)].push_maybe(editor)]
            .width(Fill)
            .into()
    }
}
