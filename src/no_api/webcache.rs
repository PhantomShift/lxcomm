use std::{
    fmt::Debug,
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
};

use chrono::DateTime;
use fjall::{
    Database, Keyspace, KeyspaceCreateOptions,
    compaction::filter::{CompactionFilter, Factory},
};
use format_bytes::format_bytes;

pub struct ExpirationFilter(chrono::DateTime<chrono::Utc>);
pub struct ExpirationFactory(Arc<AtomicU64>);

impl CompactionFilter for ExpirationFilter {
    fn filter_item(
        &mut self,
        item: fjall::compaction::filter::ItemAccessor<'_>,
        _ctx: &fjall::compaction::filter::Context,
    ) -> std::result::Result<fjall::compaction::filter::Verdict, fjall::LsmError> {
        let slice = item.value()?;
        if let Some(bytes) = slice[..8].as_array()
            && let Some(ts) = chrono::DateTime::from_timestamp_secs(i64::from_le_bytes(*bytes))
            && ts > self.0
        {
            Ok(fjall::compaction::filter::Verdict::Keep)
        } else {
            // Automatically remove entries that aren't formatted as expected
            Ok(fjall::compaction::filter::Verdict::Remove)
        }
    }
}

impl Factory for ExpirationFactory {
    fn make_filter(&self, _ctx: &fjall::compaction::filter::Context) -> Box<dyn CompactionFilter> {
        Box::new(ExpirationFilter(
            chrono::Utc::now()
                - std::time::Duration::from_secs(self.0.load(std::sync::atomic::Ordering::Relaxed)),
        ))
    }

    fn name(&self) -> &str {
        "expiration"
    }
}

#[derive(Clone)]
pub struct WebCache {
    path: PathBuf,
    db: Database,
    /// `[query] -> [timestamp_le_bytes\0HTML Page]` pair
    pages: Keyspace,
    lifetime: Arc<AtomicU64>,
}

impl Debug for WebCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Webcache({})", self.path.display())
    }
}

pub struct CacheEntry {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub page: String,
}

impl WebCache {
    pub fn new<P: Into<PathBuf>>(dir: P) -> fjall::Result<Self> {
        let path = dir.into();
        let lifetime = Arc::new(AtomicU64::new(crate::web::DEFAULT_CACHE_TIME as u64));
        let db = Database::builder(&path)
            .with_compaction_filter_factories({
                let lifetime = lifetime.clone();

                Arc::new(move |keyspace| match keyspace {
                    "pages" => Some(Arc::new(ExpirationFactory(lifetime.clone()))),
                    _ => None,
                })
            })
            .open()?;
        let pages = db.keyspace("pages", KeyspaceCreateOptions::default)?;

        Ok(Self {
            path,
            db,
            pages,
            lifetime,
        })
    }

    pub fn set_lifetime(&self, lifetime: u64) {
        self.lifetime
            .store(lifetime, std::sync::atomic::Ordering::Relaxed);
    }

    // Potential TODO: batch insert
    pub fn cache_page(&self, query: &str, body: &str) -> fjall::Result<()> {
        let now = chrono::Utc::now().timestamp().to_le_bytes();
        self.pages
            .insert(query, format_bytes!(b"{}\0{}", now, body.as_bytes()))
    }

    // Note: initially written assuming `If-Modified-Since` would work,
    // not sure if needed anywhere currently
    pub fn refresh_timestamp(&self, query: &str) -> fjall::Result<()> {
        if let Some(entry) = self.get_entry(query) {
            self.cache_page(query, &entry.page)?;
        }

        Ok(())
    }

    pub fn get_entry(&self, query: &str) -> Option<CacheEntry> {
        let slice = self.pages.get(query).ok()??;
        let timestamp =
            chrono::DateTime::from_timestamp_secs(i64::from_le_bytes(*(slice[..8].as_array()?)))?;
        let page = String::from_utf8(slice[9..].to_vec()).ok()?;

        Some(CacheEntry { timestamp, page })
    }

    pub fn get_entry_after<Tz: chrono::TimeZone>(
        &self,
        query: &str,
        dt: DateTime<Tz>,
    ) -> Option<CacheEntry> {
        self.get_entry(query).filter(|entry| entry.timestamp > dt)
    }

    pub fn remove_entry(&self, query: &str) -> fjall::Result<()> {
        self.pages.remove(query)
    }

    pub fn clear(&self) -> fjall::Result<()> {
        self.pages.clear()
    }
}
