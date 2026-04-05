use std::borrow::Borrow;

pub(super) mod btree;

pub trait KVStore<K: Ord, V>: IntoIterator {
    fn put(&mut self, key: K, value: V);

    fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized;

    fn iter<'a>(&'a self) -> impl SeekableIterator<K, Item = (&'a K, &'a V)>
    where
        K: 'a,
        V: 'a;
}

pub trait SeekableIterator<K: Ord>: Iterator {
    fn seek<Q>(&mut self, key: &Q) -> Option<Self::Item>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized;
}
