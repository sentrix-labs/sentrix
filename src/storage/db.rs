// db.rs - Sentrix Chain

use sled::Db;
use serde::{Serialize, de::DeserializeOwned};
use crate::core::blockchain::Blockchain;
use crate::types::error::{SentrixError, SentrixResult};

pub struct Storage {
    db: Db,
}

impl Storage {
    pub fn open(path: &str) -> SentrixResult<Self> {
        let db = sled::open(path)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(Self { db })
    }

    // Generic serialized put/get
    fn put<T: Serialize>(&self, key: &str, value: &T) -> SentrixResult<()> {
        let bytes = serde_json::to_vec(value)?;
        self.db.insert(key, bytes)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        self.db.flush()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    fn get<T: DeserializeOwned>(&self, key: &str) -> SentrixResult<Option<T>> {
        match self.db.get(key).map_err(|e| SentrixError::StorageError(e.to_string()))? {
            Some(bytes) => {
                let value = serde_json::from_slice(&bytes)?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    // Blockchain persistence
    pub fn save_blockchain(&self, blockchain: &Blockchain) -> SentrixResult<()> {
        self.put("blockchain", blockchain)
    }

    pub fn load_blockchain(&self) -> SentrixResult<Option<Blockchain>> {
        self.get("blockchain")
    }

    // Chain height (quick lookup without loading full chain)
    pub fn save_height(&self, height: u64) -> SentrixResult<()> {
        self.put("height", &height)
    }

    pub fn load_height(&self) -> SentrixResult<u64> {
        Ok(self.get::<u64>("height")?.unwrap_or(0))
    }

    // Check if storage has existing chain data
    pub fn has_blockchain(&self) -> bool {
        self.db.contains_key("blockchain").unwrap_or(false)
    }

    pub fn clear(&self) -> SentrixResult<()> {
        self.db.clear()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::blockchain::Blockchain;

    fn temp_db_path() -> String {
        let dir = std::env::temp_dir()
            .join(format!("sentrix_test_{}", uuid::Uuid::new_v4()));
        dir.to_str().unwrap().to_string()
    }

    #[test]
    fn test_open_storage() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();
        assert!(!storage.has_blockchain());
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_save_and_load_blockchain() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator(
            "admin",
            "val1".to_string(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        ).unwrap();

        storage.save_blockchain(&bc).unwrap();
        assert!(storage.has_blockchain());

        let loaded = storage.load_blockchain().unwrap().unwrap();
        assert_eq!(loaded.height(), bc.height());
        assert_eq!(loaded.total_minted, bc.total_minted);
        assert_eq!(loaded.chain_id, bc.chain_id);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_save_and_load_height() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        storage.save_height(42).unwrap();
        let height = storage.load_height().unwrap();
        assert_eq!(height, 42);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_persistence_across_reopen() {
        let path = temp_db_path();

        {
            let storage = Storage::open(&path).unwrap();
            let bc = Blockchain::new("admin".to_string());
            storage.save_blockchain(&bc).unwrap();
        }

        // Reopen — data should persist
        {
            let storage = Storage::open(&path).unwrap();
            assert!(storage.has_blockchain());
            let loaded = storage.load_blockchain().unwrap();
            assert!(loaded.is_some());
        }

        let _ = std::fs::remove_dir_all(&path);
    }
}
