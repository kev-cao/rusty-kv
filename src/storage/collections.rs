use std::borrow::Borrow;

pub mod btree;

pub trait MemTable {
    fn put(&mut self, key: Vec<u8>, value: Vec<u8>);

    fn get<K>(&self, key: K) -> Option<&Vec<u8>> where K: Borrow<[u8]> + Ord;

    fn iter<'a, K>(&'a self) -> impl MapIterator<'a, K> where K: Borrow<[u8]> + Ord;
}

pub trait MapIterator<'a, K: Ord> {
    type Item;

    fn seek<Q>(&mut self, key: &Q) -> Option<Self::Item> where K: Borrow<Q>, Q: Ord + ?Sized;
}
