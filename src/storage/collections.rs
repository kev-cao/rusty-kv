use std::borrow::Borrow;
use std::fmt::Debug;

pub(super) mod btree;

pub trait KVIter {
    type Key: Ord;
    type Value;
    type Error: Debug;

    fn key(&self) -> Result<Option<&Self::Key>, Self::Error>;

    fn value(&self) -> Result<Option<&Self::Value>, Self::Error>;

    /// Advances the iterator to the next key-value pair. Returns `true` if
    /// there is a next pair, or `false` if the iterator has reached the end.
    /// Must be called before calling `key()` or `value()` for the first time.
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
