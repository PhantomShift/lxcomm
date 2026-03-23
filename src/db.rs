use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Arc, Mutex},
};

use fjall::{Database, Keyspace, KeyspaceCreateOptions};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("fjall error: {0}")]
    Fjall(#[from] fjall::Error),
    #[error("(de)serialization error: {0}")]
    SerdeJSON(#[from] serde_json::Error),
}

pub trait PrimaryKey: Clone + Eq + Hash {
    fn to_bytes(&self) -> Box<[u8]>;
    fn from_bytes(bytes: &[u8]) -> Option<Self>;
}

pub trait Record: Serialize + for<'de> Deserialize<'de> {
    const PRIMARY_KEY_NAME: &'static str;
    type PrimaryKey: PrimaryKey;

    fn primary_key(&self) -> Self::PrimaryKey;
}

#[derive(Clone)]
struct AccessCache<K: Clone + Eq + Hash, V>(Arc<Mutex<HashMap<K, Arc<V>>>>);

impl<K: Clone + Eq + Hash, V> Default for AccessCache<K, V> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())))
    }
}

impl<K: Clone + Eq + Hash, V> AccessCache<K, V> {
    fn get(&self, index: &K) -> Option<Arc<V>> {
        let l = self.0.lock().expect("operations should not panic");
        let a = l.get(index)?;
        if Arc::strong_count(a) <= 1 {
            None
        } else {
            Some(a.clone())
        }
    }

    fn insert(&self, index: K, value: Arc<V>) {
        let mut l = self.0.lock().expect("operations should not panic");
        l.insert(index, value);
    }

    fn invalidate(&self, index: &K) {
        let mut l = self.0.lock().expect("operations should not panic");
        l.remove(index);
    }

    fn clear_stale(&self) {
        let mut l = self.0.lock().expect("operations should not panic");
        l.retain(|_k, v| Arc::strong_count(v) > 1);
    }
}

/// Basic persistent storage access interface
#[derive(Clone)]
pub struct DB<Item: Record> {
    db: Database,
    primary: Keyspace,
    primary_cache: AccessCache<Item::PrimaryKey, Item>,
}

impl<Item: Record> DB<Item> {
    pub fn create(db: Database) -> Result<Self, fjall::Error> {
        let primary = db.keyspace(Item::PRIMARY_KEY_NAME, KeyspaceCreateOptions::default)?;
        let primary_cache = AccessCache::default();

        Ok(Self {
            db,
            primary,
            primary_cache,
        })
    }

    pub fn store(&self, item: &Item) -> Result<(), Error> {
        let store = serde_json::to_vec(item)?;
        let pkey = item.primary_key();
        self.primary.insert(pkey.to_bytes().as_ref(), store)?;
        self.primary_cache.invalidate(&pkey);
        self.primary_cache.clear_stale();

        Ok(())
    }

    pub fn get(&self, index: Item::PrimaryKey) -> Result<Option<Arc<Item>>, Error> {
        if let Some(v) = self.primary_cache.get(&index) {
            return Ok(Some(v));
        }

        if let Some(slice) = self.get_raw(&index)? {
            let v: Item = serde_json::from_slice(&slice)?;
            let a = Arc::new(v);
            self.primary_cache.insert(index, a.clone());
            Ok(Some(a))
        } else {
            Ok(None)
        }
    }

    pub fn remove(&self, index: Item::PrimaryKey) -> Result<Option<Item>, Error> {
        if let Some(slice) = self.get_raw(&index)? {
            self.primary.remove(index.to_bytes().as_ref())?;
            self.primary_cache.invalidate(&index);

            Ok(Some(serde_json::from_slice(&slice)?))
        } else {
            Ok(None)
        }
    }

    pub fn fetch_update<F>(
        &self,
        index: Item::PrimaryKey,
        modify: F,
    ) -> Result<Option<Arc<Item>>, Error>
    where
        F: FnOnce(&mut Item),
    {
        if let Some(slice) = self.get_raw(&index)? {
            let mut v: Item = serde_json::from_slice(&slice)?;
            modify(&mut v);
            let a = Arc::new(v);
            self.primary_cache.insert(index, a.clone());
            Ok(Some(a))
        } else {
            Ok(None)
        }
    }

    /// Sorts and attempts to insert all items based on their primary key.
    pub fn bulk_store(&self, mut items: Vec<Item>) -> Result<(), Error> {
        items.sort_by_cached_key(|item| item.primary_key().to_bytes());
        // Actually not *entirely* sure if the persist call is necessary,
        // but in testing the start_ingestion call seemed to error
        // due to files not existing
        self.db.persist(fjall::PersistMode::Buffer)?;
        let mut w = self.primary.start_ingestion()?;
        for item in items.into_iter() {
            let slice = serde_json::to_vec(&item)?;
            w.write(item.primary_key().to_bytes().as_ref(), slice)?;
        }
        w.finish()?;
        Ok(())
    }

    pub fn keys(&self) -> impl Iterator<Item = Item::PrimaryKey> {
        self.primary.iter().filter_map(|g| {
            g.key()
                .ok()
                .and_then(|s| Item::PrimaryKey::from_bytes(s.as_ref()))
        })
    }

    fn get_raw(&self, index: &Item::PrimaryKey) -> Result<Option<fjall::Slice>, fjall::Error> {
        self.primary.get(index.to_bytes())
    }
}

macro_rules! PrimaryKeyImplInt {
    ($n:ty) => {
        impl PrimaryKey for $n {
            fn to_bytes(&self) -> Box<[u8]> {
                Box::new(self.to_le_bytes())
            }

            fn from_bytes(bytes: &[u8]) -> Option<Self> {
                bytes.as_array().map(|&bytes| Self::from_le_bytes(bytes))
            }
        }
    };
}

PrimaryKeyImplInt!(u64);

impl Record for workshop_reader::WorkshopFile {
    const PRIMARY_KEY_NAME: &'static str = "workshop_id";
    type PrimaryKey = u64;

    fn primary_key(&self) -> Self::PrimaryKey {
        self.published_file_id
    }
}

impl Record for workshop_reader::WorkshopCollection {
    const PRIMARY_KEY_NAME: &'static str = "workshop_id";
    type PrimaryKey = u64;

    fn primary_key(&self) -> Self::PrimaryKey {
        self.id
    }
}

mod test {
    use super::*;

    #[derive(Debug, Serialize, Deserialize)]
    struct TestItem {
        id: u32,
        title: String,
        description: Option<String>,
    }

    PrimaryKeyImplInt!(u32);

    impl Record for TestItem {
        const PRIMARY_KEY_NAME: &'static str = "ids";
        type PrimaryKey = u32;

        fn primary_key(&self) -> Self::PrimaryKey {
            self.id
        }
    }

    #[test]
    fn basic_test() -> Result<(), Error> {
        let mut path = std::env::temp_dir();
        path.push("lxcomm_db_test");

        let db = Database::builder(path).temporary(true).open()?;
        let items: DB<TestItem> = DB::create(db.clone())?;
        items.store(&TestItem {
            id: 0,
            title: "Test".into(),
            description: None,
        })?;
        println!("{:?}", items.get(0));
        println!(
            "{:?}",
            items.fetch_update(0, |item| item.description = Some("Test description".into()))?
        );
        items.bulk_store(vec![
            TestItem {
                id: 42,
                title: "Haha 42".into(),
                description: Some("The universal answer".into()),
            },
            TestItem {
                id: 6,
                title: "Why was it afraid of 7".into(),
                description: Some("haha 67".into()),
            },
        ])?;

        println!("{:?}", items.remove(6)?);

        assert_eq!(2, items.keys().count());

        Ok(())
    }
}
