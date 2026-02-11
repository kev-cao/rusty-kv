use std::borrow::Borrow;

pub mod btree;

pub trait KVStore<K: Ord, V>: IntoSeekableIterator<K> {
    fn put(&mut self, key: K, value: V);

    fn get<Q>(&self, key: &Q) -> Option<&V> where K: Borrow<Q>, Q: Ord + ?Sized;

    fn iter<'a>(&'a self) -> impl SeekableIterator<K>;
}

pub trait IntoSeekableIterator<K: Ord>: IntoIterator {
    type IntoIter: SeekableIterator<K>;
}

pub trait SeekableIterator<K: Ord>: Iterator {
    fn seek<Q>(&mut self, key: &Q) -> Option<Self::Item> where K: Borrow<Q>, Q: Ord + ?Sized;
}
