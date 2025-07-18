use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestWorkshopItem {
    pub size: u64,
    #[serde(rename = "timeupdated")]
    pub time_updated: u64,
    pub manifest: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestWorkshopItemDetails {
    pub manifest: String,
    #[serde(rename = "timeupdated")]
    pub time_updated: u64,
    #[serde(rename = "timetouched")]
    pub time_touched: u64,
    #[serde(rename = "BytesDownloaded")]
    #[serde(default)]
    pub bytes_downloaded: u64,
    #[serde(rename = "BytesToDownload")]
    #[serde(default)]
    pub bytes_to_download: u64,
    #[serde(rename = "latest_timeupdated")]
    pub latest_time_updated: u64,
    #[serde(rename = "latest_manifest")]
    pub latest_manifest: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[cfg_attr(debug_assertions, serde(deny_unknown_fields))]
pub struct AppWorkshopManifest {
    #[serde(rename = "appid")]
    pub app_id: u64,
    pub size_on_disk: u64,
    pub needs_update: bool,
    pub needs_download: bool,
    pub time_last_updated: u64,
    pub time_last_app_ran: u64,
    #[serde(rename = "LastBuildID")]
    pub last_build_id: u64,
    pub workshop_items_installed: BTreeMap<u64, ManifestWorkshopItem>,
    pub workshop_item_details: BTreeMap<u64, ManifestWorkshopItemDetails>,
}

#[test]
fn test_serde() {
    let manifest: AppWorkshopManifest =
        keyvalues_serde::from_str(include_str!("../test/appworkshop_268500.acf"))
            .expect("failed to deserialize manifest file");
    let to_download = manifest
        .workshop_item_details
        .get(&3187313670)
        .map(|details| details.bytes_to_download);
    assert_eq!(Some(300914768), to_download);
}
