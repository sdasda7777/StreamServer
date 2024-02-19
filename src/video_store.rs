
//! This module provides VideoStorage and VideoFile.

use std::sync::Arc;
use std::collections::HashMap;
use futures::Stream;
use parking_lot::RwLock;
use tst::TSTSet;

use async_broadcast::{Sender, Receiver};

/// VideoFile just stores the content type and original broadcast channel receiver.
pub struct VideoFile {
    /// MIME content type of the file, as received in the "Content-Type" header
    content_type: String,
    /// broadcast receiver pointing at the beginning of the file
    receiver: Receiver<Vec<u8>>,
}

impl VideoFile {
    /// Associated function for creating new file
    pub fn new_with_handle(
        content_type: String,
        file_blocks_no: usize,
    ) -> (Sender<Vec<u8>>, Arc<Self>) {
        let (sender, receiver) = async_broadcast::broadcast(file_blocks_no);
        (
            sender,
            Arc::new(Self {
                content_type,
                receiver,
            })
        )
    }
    /// Method for obtaining content type of the file
    pub fn content_type(&self) -> &str {
        &self.content_type
    }
    /// Method for obtaining Stream of the whole file
    pub fn data_stream(&self) -> impl Stream<Item=Vec<u8>> {
        self.receiver.clone()
    }
}

/// Type of reference to configuration
type ConfigRef = Arc<RwLock<VideoStoreConfig>>;
/// Type of reference to a video file
type FileRef = Arc<VideoFile>;
/// Type of reference to main data storage type
type StorageRef = Arc<RwLock<HashMap<String, FileRef>>>;
/// Type of reference to listing data storage type
type TrieRef = Arc<RwLock<TSTSet>>;

/// VideoStorage uses HashMap for storing VideoFiles and a prefix tree for search in prefixes.
#[derive(Clone)]
pub struct VideoStore {
    /// Reference to the configuration
    config: ConfigRef,
    /// Reference to the main storage
    data_storage: StorageRef,
    /// Reference to the listing storage
    available_paths: TrieRef,
}

/// VideoStoreConfig contains server configuration
pub struct VideoStoreConfig {
    /// Number of blocks that should be allocated in a file
    pub file_allocated_blocks_no: usize,
    /// Preferred size of a block
    ///   (in reality blocks will resize to fit chunks completely)
    pub file_preferred_block_size: usize,
}

/// VideoStore abstracts file creation, storage, deletion and search,
///   as well as runtime configuration.
impl VideoStore {
    /// Associated function for used to create a new VideoStore
    pub fn new(config: VideoStoreConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            data_storage: Arc::new(RwLock::new(HashMap::new())),
            available_paths: Arc::new(RwLock::new(TSTSet::new())),
        }
    }
    
    /// Method for obtaining a read guard of the configuration object.
    pub fn config(&self) -> impl core::ops::Deref<Target=VideoStoreConfig> + '_ {
        self.config.read()
    }
    
    /// Put a new file with specified content type at specified path,
    ///   return handle for writing to the file.
    pub fn create(&self, path: String, content_type: String) -> Sender<Vec<u8>> {
        let (handle, file) = VideoFile::new_with_handle(
            content_type,
            self.config.read().file_allocated_blocks_no,
        );
        self.available_paths.write().insert(&path);
        self.data_storage.write().insert(path, file);
        handle
    }
    
    /// Try retrieving a file, return resulting option
    pub fn get(&self, path: &str) -> Option<FileRef> {
        self.data_storage.read().get(path).cloned()
    }
    
    /// Try removing a file, return Some(()) on success, None on failure
    pub fn remove(&self, path: &str) -> Option<()> {
        self.available_paths.write().remove(path);
        self.data_storage.write().remove(path).map(|_| ())
    }
    
    /// List stored files.
    /// To list multiple files, path must contain '*' as the last character.
    pub fn list(&self, path: &str) -> Vec<String> {
        // When wildcard is present, search using prefix set, otherwise use the HashMap
        if path.contains('*') {
            if path.len() == 1 {
                self.available_paths.read().iter().collect()
            } else {
                self.available_paths.read().prefix_iter(&path[0..path.len()-1]).collect()
            }
        } else {
            match self.data_storage.read().get(path) {
                None => Vec::new(),
                Some(_) => vec![path.to_string()],
            }
        }
    }
}

