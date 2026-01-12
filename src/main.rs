mod storage;

use std::collections::BTreeMap;

struct KvStore {
    store: BTreeMap<Vec<u8>, Vec<u8>>
}

impl KvStore {
    pub fn new() -> Self {
        KvStore {
            store: BTreeMap::new()
        }
    }

    pub fn set(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.store.insert(key, value);
    }

    pub fn get(&self, key: Vec<u8>) -> Option<&Vec<u8>> {
        self.store.get(&key)
    }
}

fn main() {
    println!("Hello, world!");
}
