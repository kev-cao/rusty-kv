use std::borrow::Borrow;

pub mod btree;

pub trait MemTable {
    fn put(&mut self, key: Vec<u8>, value: Vec<u8>);

    fn get<K: Borrow<[u8]>>(&self, key: K) -> Option<&Vec<u8>>;

    fn iter<'a>(&'a self) -> impl MapIterator<'a, Key = Vec<u8>, Value = Vec<u8>>;
}

pub trait MapIterator<'a> {
    type Key: Ord;
    type Value;

    fn next(&mut self) -> Option<(&'a Self::Key, &'a Self::Value)>;
}
