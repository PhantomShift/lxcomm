use std::{borrow::Cow, collections::HashSet, path::Path};

use dashmap::DashMap;

use crate::{
    ACTIVE_CONFIG_DIR, ACTIVE_MODS_DIR, PROFILES_DIR, files,
    library::{self, Profile},
    xcom_mod::{self, ModId},
};

#[non_exhaustive]
pub struct LoadSettings {
    pub linux_native_mode: bool,
}

pub fn build_active_config<D: AsRef<Path>>(
    download_dir: D,
    profile: &Profile,
) -> Result<(), std::io::Error> {
    let profile_path = PROFILES_DIR.join(&profile.name);

    if !profile_path.try_exists()? {
        std::fs::create_dir(&profile_path)?;
        eprintln!(
            "Warning: created new profile folder; if this is not a fresh profile, something has gone wrong."
        );
    }

    if ACTIVE_CONFIG_DIR.try_exists()? {
        std::fs::remove_dir_all(ACTIVE_CONFIG_DIR.as_path())?;
    }

    std::fs::create_dir_all(ACTIVE_CONFIG_DIR.as_path())?;

    for (item, settings) in profile.items.iter() {
        if !settings.enabled {
            continue;
        }

        let id_string = item.get_hash();
        let item_config = ACTIVE_CONFIG_DIR.join(&id_string);
        std::fs::create_dir_all(&item_config)?;
        let mut custom_configs = HashSet::new();
        let item_profile_config = profile_path.join(id_string);
        if item_profile_config.try_exists()? {
            for file in std::fs::read_dir(item_profile_config)? {
                let file = file?;

                if let Some(ext) = file.path().extension()
                    && ext == "ini"
                {
                    let file_path = file.path().canonicalize()?;
                    let name = file_path
                        .file_name()
                        .expect("file name should exist if the extension exists");
                    custom_configs.insert(name.to_owned());
                    files::link_files(&file_path, item_config.join(name))?;
                }
            }
        }

        let original_config = files::get_mod_config_directory(download_dir.as_ref(), item);
        if original_config.try_exists()? {
            for file in std::fs::read_dir(original_config)? {
                let file = file?;
                if let Some(ext) = file.path().extension()
                    && ext == "ini"
                {
                    let file_path = file.path().canonicalize()?;
                    let name = file_path
                        .file_name()
                        .expect("file name should exist if the extension exists");
                    if !custom_configs.contains(name) {
                        files::link_files(&file_path, item_config.join(name))?;
                    }
                }
                // TODO - Allow files in subdirectories to be edited as well
                else if file.file_type()?.is_dir() {
                    let path = file.path().canonicalize()?;
                    files::link_dirs(path, item_config.join(file.file_name()))?;
                }
            }
        }
    }

    Ok(())
}

pub fn build_mod_environment<D: AsRef<Path>>(
    download_dir: D,
    metadata: &DashMap<ModId, xcom_mod::ModMetadata>,
    profile: &Profile,
) -> Result<(), std::io::Error> {
    build_active_config(download_dir.as_ref(), profile)?;

    if ACTIVE_MODS_DIR.try_exists()? {
        std::fs::remove_dir_all(ACTIVE_MODS_DIR.as_path())?;
    }

    std::fs::create_dir_all(ACTIVE_MODS_DIR.as_path())?;

    for id in profile.items.keys() {
        let Some(data) = metadata.get(id) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("item information missing for id {id}"),
            ));
        };

        let id_string = id.get_hash();
        let files = files::get_mod_directory(download_dir.as_ref(), id);

        let dest = ACTIVE_MODS_DIR.join(&data.dlc_name);

        if dest.try_exists()? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Mod with DLCName {} is defined twice", &data.dlc_name),
            ));
        }

        std::fs::create_dir_all(&dest)?;

        for entry in std::fs::read_dir(files)? {
            let mut path = entry?.path();
            let name = path
                .file_name()
                .expect("paths retrieved from read_dir should not be '..'")
                .to_owned();

            if name.eq_ignore_ascii_case("config") && ACTIVE_CONFIG_DIR.join(&id_string).exists() {
                path = ACTIVE_CONFIG_DIR.join(&id_string);
            }

            // Skip source files
            if name.eq_ignore_ascii_case("source") || name.eq_ignore_ascii_case("src") {
                continue;
            }

            let dest = dest.join(name);

            if path.is_dir() {
                files::link_dirs(path, dest)?;
            } else if path.is_file() {
                files::link_files(path, dest)?;
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unable to process file",
                ));
            }
        }
    }

    Ok(())
}

pub fn write_mod_list<W: std::io::Write>(
    profile: &Profile,
    metadata: &DashMap<ModId, xcom_mod::ModMetadata>,
    mut writer: W,
) -> Result<(), std::io::Error> {
    writeln!(writer, "[Engine.XComModOptions]")?;

    let mut len = 0;
    let mut lines = Vec::new();

    for id in profile.items.keys() {
        let Some(data) = metadata.get(id) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "mod metadata was missing",
            ));
        };

        let b = format!("ActiveMods={}\n", data.dlc_name).into_bytes();
        len += b.len();
        lines.push(b);
    }
    lines.sort();
    let mut buf = Vec::with_capacity(len);
    buf.extend(lines.into_iter().flatten());
    writer.write_all(&buf)?;
    Ok(())
}

pub fn mod_list(
    profile: &Profile,
    metadata: &DashMap<ModId, xcom_mod::ModMetadata>,
) -> Result<String, std::io::Error> {
    let mut bytes = Vec::new();
    write_mod_list(profile, metadata, &mut bytes)?;

    String::from_utf8(bytes)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

pub fn link_mod_environment<D: AsRef<Path>>(
    profile: &Profile,
    settings: &LoadSettings,
    metadata: &DashMap<ModId, xcom_mod::ModMetadata>,
    destination: D,
) -> Result<(), std::io::Error> {
    if !ACTIVE_MODS_DIR.try_exists()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "active mods directory not found",
        ));
    }
    if !ACTIVE_CONFIG_DIR.try_exists()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "active config directory not found",
        ));
    }

    let lower_linux = move |s: &'static str| -> Cow<'static, str> {
        if settings.linux_native_mode {
            Cow::Owned(s.to_lowercase())
        } else {
            Cow::from(s)
        }
    };

    let xcom_game_dir = destination.as_ref().join(lower_linux("XComGame").as_ref());
    let mods_dest = xcom_game_dir.join(lower_linux("Mods").as_ref());
    if mods_dest.try_exists()? {
        let backup = mods_dest.with_added_extension("bak");
        if !backup.try_exists()? {
            eprintln!("Moving existing mods directory to {}...", backup.display());
            std::fs::rename(&mods_dest, backup)?;
        }
        if mods_dest.exists() {
            std::fs::remove_dir_all(&mods_dest)?;
        }
    } else if let Ok(metadata) = std::fs::symlink_metadata(&mods_dest)
        && metadata.is_symlink()
    {
        eprintln!(
            "Removing existing (likely broken) symlink at {}...",
            mods_dest.display()
        );
        std::fs::remove_dir_all(&mods_dest)?;
    }

    let config_dest = xcom_game_dir.join(lower_linux("Config").as_ref());
    if !config_dest.try_exists()? {
        std::fs::create_dir(&config_dest)?;
    }
    let default_mod_options = config_dest.join(lower_linux("DefaultModOptions.ini").as_ref());
    if default_mod_options.try_exists()?
        && !default_mod_options
            .with_added_extension("bak")
            .try_exists()?
    {
        std::fs::copy(
            &default_mod_options,
            default_mod_options.with_added_extension("bak"),
        )?;
    }

    write_mod_list(
        profile,
        metadata,
        std::fs::File::create(default_mod_options)?,
    )?;

    files::link_dirs(ACTIVE_MODS_DIR.as_path(), mods_dest)?;

    Ok(())
}

pub fn link_profile_local_files<L: AsRef<Path>>(
    profile: &Profile,
    metadata: &DashMap<ModId, xcom_mod::ModMetadata>,
    local_path: L,
) -> Result<(), std::io::Error> {
    // Validate that this is actually the game's local documents folder
    macro_rules! ensure {
        ($expression:expr, $msg:expr$(,)*) => {
            if !($expression) {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, $msg));
            };
        };
    }
    let path_display = local_path.as_ref().display();
    ensure!(
        local_path
            .as_ref()
            .file_name()
            .is_some_and(|name| name == "XComGame"),
        format!(
            "{path_display} does not end with 'XComGame', this does not appear to be to be the save folder",
        ),
    );
    ensure!(
        local_path.as_ref().join("Logs").exists(),
        format!(
            "cannot find 'Logs' folder in {path_display}, please launch the game at least once if this is the correct folder"
        )
    );
    ensure!(
        local_path.as_ref().exists(),
        format!("could not find or read {path_display}")
    );

    let profile_path = PROFILES_DIR.join(&profile.name);
    if !profile_path.try_exists()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "profile folder does not exist",
        ));
    }

    let link_profile_folder = |name: &str| -> Result<(), std::io::Error> {
        let destination = local_path.as_ref().join(name);
        let backup = destination.with_extension("bak");
        if destination.try_exists()?
            && !destination.is_symlink()
            && destination.is_dir()
            && !backup.exists()
        {
            eprintln!(
                "Moving {} to {}...",
                destination.display(),
                backup.display()
            );
            std::fs::rename(&destination, backup)?;
        }

        if destination.try_exists()? {
            std::fs::remove_dir_all(&destination).map_err(|e| {
                std::io::Error::other(format!(
                    "Failed to remove '{}': {}",
                    destination.display(),
                    e
                ))
            })?;
        }

        let source = profile_path.join(name);
        if !source.try_exists()? {
            std::fs::create_dir(&source)?;
        }

        files::link_dirs(source, destination)?;

        Ok(())
    };

    link_profile_folder(library::profile_folder::CHARACTER_POOL)?;
    link_profile_folder(library::profile_folder::CONFIG)?;
    link_profile_folder(library::profile_folder::PHOTOBOOTH)?;
    link_profile_folder(library::profile_folder::SAVE_DATA)?;

    // This fix was added for native Linux version, but it should be safely applicable to Windows/Proton version
    let config_dir = local_path.as_ref().join(library::profile_folder::CONFIG);
    let xcom_mod_options = std::fs::File::create(config_dir.join("XComModOptions.ini"))?;
    write_mod_list(profile, metadata, xcom_mod_options)?;

    Ok(())
}

// All necessary steps to load a profile into the game directory
pub fn bootstrap_load_profile<DL: AsRef<Path>, DS: AsRef<Path>, L: AsRef<Path>>(
    profile: &Profile,
    settings: LoadSettings,
    download_dir: DL,
    metadata: &DashMap<ModId, xcom_mod::ModMetadata>,
    destination: DS,
    local_path: L,
) -> Result<(), std::io::Error> {
    build_mod_environment(download_dir.as_ref(), metadata, profile)?;
    link_mod_environment(profile, &settings, metadata, destination.as_ref())?;
    link_profile_local_files(profile, metadata, local_path.as_ref())?;
    Ok(())
}
