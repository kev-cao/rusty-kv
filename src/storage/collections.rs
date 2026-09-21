use std::borrow::Borrow;
use std::fmt::Debug;

pub(super) mod btree;

pub trait KVIterator {
    type Key: Ord;
    type Value;
    type Error: Debug;

    fn key(&self) -> Result<Option<&Self::Key>, Self::Error>;

    fn value(&self) -> Result<Option<&Self::Value>, Self::Error>;

    fn next(&mut self) -> Result<bool, Self::Error>;

    fn seek_ge<Q>(&mut self, key: &Q) -> Result<bool, Self::Error>
    where
        Self::Key: Borrow<Q>,
        Q: Ord + ?Sized;

    fn seek_lt<Q>(&mut self, key: &Q) -> Result<bool, Self::Error>
    where
        Self::Key: Borrow<Q>,
        Q: Ord + ?Sized;
}
