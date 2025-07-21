use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};

use crate::{ACTIVE_CONFIG_DIR, ACTIVE_MODS_DIR, PROFILES_DIR, files, library::Profile, xcom_mod};

pub fn build_active_config<D: AsRef<Path>>(
    download_dir: D,
    profile: &Profile,
) -> Result<(), std::io::Error> {
    let profile_path = PROFILES_DIR.join(profile.id.to_string());

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

        let id_string = item.to_string();
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

        let original_config = files::get_item_config_directory(download_dir.as_ref(), *item);
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
            }
        }
    }

    Ok(())
}

pub fn build_mod_environment<D: AsRef<Path>>(
    download_dir: D,
    metadata: &BTreeMap<u32, xcom_mod::ModMetadata>,
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
                format!("item informatin missing for id {id}"),
            ));
        };

        let id_string = id.to_string();
        let files = files::get_item_directory(download_dir.as_ref(), *id);

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
    metadata: &BTreeMap<u32, xcom_mod::ModMetadata>,
    mut writer: W,
) -> Result<(), std::io::Error> {
    writeln!(writer, "[Engine.XComModOptions]")?;
    writeln!(writer)?;

    for id in profile.items.keys() {
        let Some(data) = metadata.get(id) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "mod metadata was missing",
            ));
        };

        writeln!(writer, r#"ActiveMods="{}""#, data.dlc_name)?;
    }
    Ok(())
}

pub fn mod_list(
    profile: &Profile,
    metadata: &BTreeMap<u32, xcom_mod::ModMetadata>,
) -> Result<String, std::io::Error> {
    let mut bytes = Vec::new();
    write_mod_list(profile, metadata, &mut bytes)?;

    String::from_utf8(bytes)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

pub fn link_mod_environment<D: AsRef<Path>>(
    profile: &Profile,
    metadata: &BTreeMap<u32, xcom_mod::ModMetadata>,
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

    let xcom_game_dir = destination.as_ref().join("XComGame");
    let mods_dest = xcom_game_dir.join("Mods");
    if mods_dest.try_exists()? {
        let backup = mods_dest.with_added_extension("bak");
        if !backup.try_exists()? {
            eprintln!("Moving existing mods directory to {}...", backup.display());
            dircpy::copy_dir(&mods_dest, backup)?;
        }
        std::fs::remove_dir_all(&mods_dest)?;
    } else if let Ok(metadata) = std::fs::symlink_metadata(&mods_dest)
        && metadata.is_symlink()
    {
        eprintln!(
            "Removing existing (likely broken) symlink at {}...",
            mods_dest.display()
        );
        std::fs::remove_dir_all(&mods_dest)?;
    }

    let config_dest = xcom_game_dir.join("Config");
    if !config_dest.try_exists()? {
        std::fs::create_dir(&config_dest)?;
    }
    let default_mod_options = config_dest.join("DefaultModOptions.ini");
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
    local_path: L,
) -> Result<(), std::io::Error> {
    // Validate that this is actually inside the documents folder
    let mut search = local_path.as_ref().components();
    let mut validate_has = |name: &'static str| {
        search
            .find(|comp| comp.as_os_str().eq_ignore_ascii_case(name))
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "did not find '{name}' in given local path, it is unlikely the game points here"
                ),
            ))
    };
    validate_has("Documents")?;
    validate_has("My Games")?;

    if !local_path.as_ref().exists() {
        eprintln!("Local directory doesn't appear to exist, creating it from scratch...");
        std::fs::create_dir_all(local_path.as_ref())?;
    }

    let save_path = local_path.as_ref().join("SaveData");

    if save_path.try_exists()? && save_path.is_dir() && !save_path.with_extension("bak").exists() {
        eprintln!(
            "moving {} to {}...",
            save_path.display(),
            save_path.with_extension("bak").display()
        );
        dircpy::copy_dir(&save_path, save_path.with_extension("bak"))?;
    }

    std::fs::remove_dir_all(&save_path)?;

    let profile_save = PROFILES_DIR.join(profile.id.to_string()).join("SaveData");
    if !profile_save.try_exists()? {
        std::fs::create_dir(&profile_save)?;
    }

    files::link_dirs(profile_save, save_path)?;

    Ok(())
}

// All necessary steps to load a profile into the game directory
pub fn bootstrap_load_profile<DL: AsRef<Path>, DS: AsRef<Path>, L: AsRef<Path>>(
    profile: &Profile,
    download_dir: DL,
    metadata: &BTreeMap<u32, xcom_mod::ModMetadata>,
    destination: DS,
    local_path: L,
) -> Result<(), std::io::Error> {
    build_mod_environment(download_dir.as_ref(), metadata, profile)?;
    link_mod_environment(profile, metadata, destination.as_ref())?;
    link_profile_local_files(profile, local_path.as_ref())?;
    Ok(())
}
