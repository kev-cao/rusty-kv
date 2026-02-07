use std::borrow::Borrow;
use std::iter::{IntoIterator, Iterator};
use std::mem;
use std::vec;

enum Node<K: Ord, V> {
    LNode(LNode<K, V>),
    INode(INode<K,V>),
}

impl<K: Ord, V> Node<K, V> {
    /// Fetches the value associated with the provided key, if it exists.
    fn get<Q>(&self, key: &Q) -> Option<&V>
        where K: Borrow<Q>,
              Q: Ord + ?Sized
    {
        match self {
            Node::LNode(lnode) => lnode.get(key),
            Node::INode(inode) => inode.get(key)
        }
    }

    /// Inserts a key/value pair into the node.
    fn put(&mut self, key: K, value: V, degree: usize) -> PutResult<K, V> {
        match self {
            Node::LNode(lnode) => lnode.put(key, value, degree),
            Node::INode(inode) => inode.put(key, value, degree),
        }
    }
}

impl <K: Ord, V> Default for Node<K, V> {
    fn default() -> Self {
        Self::LNode(LNode{kvs: Vec::new() })
    }
}

#[derive(Default)]
struct LNode<K: Ord, V> {
    kvs: Vec<(K, V)>
}

impl<K: Ord, V> LNode<K, V> {
    /// Returns true if the leaf node has reached the maximum capacity and
    /// needs to be split.
    fn should_split(&self, degree:usize) -> bool {
        self.kvs.len() >= 2 * degree
    }

    /// Fetches the value associated with the provided key, if it exists.
    fn get<Q>(&self, key: &Q) -> Option<&V>
        where K: Borrow<Q>,
              Q: Ord + ?Sized
    {
        self.kvs.binary_search_by(|(probe, _)| probe.borrow().cmp(key))
            .ok()
            .map(|idx| &self.kvs[idx].1)
    }

    /// Inserts a key/value pair into the node.
    fn put(&mut self, key: K, value: V, degree: usize) -> PutResult<K, V> {
        match self.kvs.binary_search_by(|(probe, _)| probe.cmp(&key)) {
            Ok(idx) => self.kvs[idx].1 = value,
            Err(idx) => self.kvs.insert(idx, (key, value))
        }
        if !self.should_split(degree) {
            return PutResult::Success
        }
        let (promoted_kv, right_sibling)= self.split();
        PutResult::Split{
            promoted_kv,
            right_sibling: Node::LNode(right_sibling)
        }
    }

    /// Splits the leaf node at the mid point. This assumes that the leaf node
    /// needs to be split.
    ///
    /// # Returns
    /// The extracted median KV, and the right sibling.
    fn split(&mut self) -> ((K, V), LNode<K, V>){
        let mid = self.kvs.len() / 2;
        let right_half = self.kvs.split_off(mid);
        let mid_kv = self.kvs
            .pop()
            .expect("should not happen: node with split with less than 3 elements");
        (mid_kv, LNode{kvs: right_half})
    }
}

#[derive(Default)]
struct INode<K: Ord, V> {
    /// Always one more than the number of KVs.
    children: Vec<Node<K, V>>,
    kvs: Vec<(K, V)>,
}

impl<K: Ord, V> INode<K, V> {
    /// Returns true if the internal node has reached the maximum capacity and
    /// needs to be split.
    fn should_split(&self, degree:usize) -> bool {
        self.kvs.len() >= 2 * degree
    }


    /// Fetches the value associated with the provided key, if it exists.
    fn get<Q>(&self, key: &Q) -> Option<&V>
        where K: Borrow<Q>,
              Q: Ord + ?Sized
    {
        match self.kvs.binary_search_by(|(probe, _)| probe.borrow().cmp(key)) {
            Ok(idx) => Some(&self.kvs[idx].1),
            Err(idx) => self.children[idx].get(key),
        }
    }

    /// Inserts a key/value pair into the node.
    fn put(&mut self, key: K, value: V, degree: usize) -> PutResult<K, V> {
        let Err(ins_idx) = self.kvs.binary_search_by(|(probe, _)| probe.cmp(&key)) else {
            return PutResult::Success
        };

        let put_result = self.children[ins_idx].put(key, value, degree);
        let PutResult::Split { promoted_kv, right_sibling  } = put_result else {
            return put_result;
        };

        self.kvs.insert(ins_idx, promoted_kv);
        self.children.insert(ins_idx + 1, right_sibling);

        if !self.should_split(degree) {
            return PutResult::Success;
        }

        let (promoted_kv, right_sibling) = self.split();
        PutResult::Split{
            promoted_kv,
            right_sibling: Node::INode(right_sibling),
        }
    }

    /// Splits the internal node at the mid point. This assumes that the
    /// internal node needs to be split.
    ///
    /// # Returns
    /// The extracted median KV, and the right sibling.
    fn split(&mut self) -> ((K, V), INode<K, V>){
        let mid = self.kvs.len() / 2;
        let right_kvs = self.kvs.split_off(mid);
        let right_children = self.children.split_off(mid);
        let promoted_kv = self.kvs
            .pop()
            .expect("should not happen; split occurred with less than 3 elements");
        let right_sibling = INode{
            kvs: right_kvs,
            children: right_children,
        };
        (promoted_kv, right_sibling)
    }
}

enum PutResult<K: Ord, V> {
    Success,
    Split {
        promoted_kv: (K, V),
        right_sibling: Node<K, V>
    }
}

pub struct BTree<K: Ord, V> {
    pub degree: usize,
    root: Node<K, V>,
}

impl<K: Ord, V> BTree<K, V> {
    /// Constructs a new btree with the provided degree.
    pub fn new(degree: usize) -> Self {
        Self {
            degree,
            root: Node::LNode(LNode {
                kvs: Vec::new()
            })
        }
    }

    /// Returns true if the tree contains no elements.
    pub fn is_empty(&self) -> bool {
        match &self.root {
            Node::LNode(lnode) => lnode.kvs.is_empty(),
            Node::INode(..) => false
        }
    }

    /// Fetches the value associated with the key in the tree, if it exists.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
        where K: Borrow<Q>,
              Q: Ord + ?Sized
    {
        self.root.get(key)
    }

    /// Inserts a key value pair into the tree. If the key already exists,
    /// updates the value mapped to the key.
    pub fn put(&mut self, key: K, value: V) {
        let result = self.root.put(key, value, self.degree);
        let PutResult::Split {promoted_kv, right_sibling} = result else { return };
        // Replace with dummy value.
        let old_root = mem::replace(&mut self.root, Node::LNode(LNode{kvs: Vec::new()}));
        let new_root = Node::INode(INode{
            kvs: vec![promoted_kv],
            children: vec![old_root, right_sibling],
        });
        self.root = new_root;
    }

    pub fn iter(&self) -> BTreeIter<'_, K, V> {
        BTreeIter::new(self)
    }
}

impl<K: Ord, V> IntoIterator for BTree<K, V> {
    type Item = (K, V);
    type IntoIter = OwnedBTreeIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        OwnedBTreeIter::new(self)
    }
}

enum IterFrame<'a, K: Ord, V> {
    LNode(&'a LNode<K, V>, usize),
    INode(&'a INode<K, V>, usize)
}

impl<'a, K: Ord, V> IterFrame<'a, K, V> {
    /// Wraps a BTree Node in an IterFrame.
    fn from_node(node: &'a Node<K, V>) -> Self {
        match node {
            Node::LNode(lnode) => Self::LNode(lnode, 0),
            Node::INode(inode) => Self::INode(inode, 0),
        }
    }
}

pub struct BTreeIter<'a, K: Ord, V> {
    stack: Vec<IterFrame<'a, K, V>>
}

impl <'a, K: Ord, V> BTreeIter<'a, K, V> {
    /// Constructs a new iterator from a BTree.
    fn new(btree: &'a BTree<K, V>) -> Self {
        Self {
            stack: vec![IterFrame::from_node(&btree.root)]
        }
    }
}

impl <'a, K: Ord, V> Iterator for BTreeIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    /// Returns the next key/value pair in the BTree.
    fn next(&mut self) -> Option<Self::Item> {
        let mut frame = self.stack.pop()?;

        match frame {
            IterFrame::LNode(lnode, ref mut cursor) => {
                let (k, v) = &lnode.kvs[*cursor];
                *cursor += 1;
                if *cursor != lnode.kvs.len() {
                    self.stack.push(frame);
                }
                Some((k, v))
            },
            IterFrame::INode(inode, ref mut cursor) => {
                if *cursor % 2 == 0 {
                    let to_push = *cursor / 2;
                    *cursor += 1;
                    if *cursor <= inode.kvs.len() * 2 {
                        self.stack.push(frame)
                    }
                    self.stack.push(IterFrame::from_node(&inode.children[to_push]));
                    self.next()
                } else {
                    let (k, v) = &inode.kvs[*cursor / 2];
                    *cursor += 1;
                    self.stack.push(frame);
                    Some((k, v))
                }
            }
        }
    }
}

enum OwnedFrame<K: Ord, V> {
    LNode(vec::IntoIter<(K, V)>),
    INode {
        kvs: vec::IntoIter<(K, V)>,
        children: vec::IntoIter<Node<K, V>>,
        next_is_child: bool,
    },
}

pub struct OwnedBTreeIter<K: Ord, V> {
    stack: Vec<OwnedFrame<K, V>>
}

impl <K: Ord, V> OwnedBTreeIter<K, V> {
    fn new(btree: BTree<K, V>) -> Self {
        let mut s = Self{stack: Vec::new()};
        s.push_node(btree.root);
        s
    }

    fn push_node(&mut self, node: Node<K, V>) {
        match node {
            Node::LNode(lnode) => self.stack.push(OwnedFrame::LNode(lnode.kvs.into_iter())),
            Node::INode(inode) => self.stack.push(OwnedFrame::INode{
                kvs: inode.kvs.into_iter(),
                children: inode.children.into_iter(),
                next_is_child: true,
            })
        }
    }
}

impl <K: Ord, V> Iterator for OwnedBTreeIter<K, V> {
    type Item = (K, V);

    fn next(&mut self) -> Option<Self::Item> {
        let mut frame = self.stack.pop()?;

        match frame {
            OwnedFrame::LNode(ref mut lnode) => {
                match lnode.next() {
                    Some(kv) => {
                        self.stack.push(frame);
                        Some(kv)
                    },
                    None => {
                        self.next()
                    }
                }
            },
            OwnedFrame::INode{
                ref mut kvs,
                ref mut children,
                ref mut next_is_child,
            } => {
                let push_child = *next_is_child;
                *next_is_child = !*next_is_child;
                if push_child {
                    match children.next() {
                        None => self.next(),
                        Some(c) => {
                            self.stack.push(frame);
                            self.push_node(c);
                            self.next()
                        }
                    }
                } else {
                    let Some(kv) = kvs.next() else {
                        return self.next();
                    };
                    self.stack.push(frame);
                    Some(kv)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rng;
    use rand::seq::SliceRandom;

    #[test]
    fn put_get_roundtrip() {
        const DEGREE: usize = 4;
        const NUM_VALUES: i32 = 100;
        let mut tree = BTree::<i32, String>::new(DEGREE);
        let mut keys: Vec<i32> = (1..=NUM_VALUES).collect();
        keys.shuffle(&mut rng());

        for k in &keys {
            tree.put(*k, k.to_string());
            assert_eq!(tree.get(k), Some(&k.to_string()));
        }

        for k in &keys {
            assert_eq!(tree.get(k), Some(&k.to_string()));
        }

        assert_eq!(tree.get(&(NUM_VALUES + 1)), None);
    }

    #[test]
    fn iter_next() {
        let degree = rand::random_range(2..10);
        println!("using degree {}", degree);
        const NUM_VALUES: i32 = 100;
        #[derive(Debug)]
        enum InsertOrder {
            Ordered,
            Reverse,
            Random
        }
        let orders = [InsertOrder::Ordered, InsertOrder::Reverse, InsertOrder::Random];
        for order in orders {
            println!("inserting in {:?} order", order);
            let mut tree = BTree::<i32, i32>::new(degree);
            let to_insert: Vec<i32> = match order {
                InsertOrder::Ordered => (1..=NUM_VALUES).collect(),
                InsertOrder::Reverse => (1..=NUM_VALUES).rev().collect(),
                InsertOrder::Random => {
                    let mut v: Vec<i32> = (1..=NUM_VALUES).collect();
                    v.shuffle(&mut rng());
                    v
                }
            };
            for x in to_insert {
                tree.put(x, x);
            }

            let mut it = tree.iter();
            for x in 1..=NUM_VALUES {
                let next = it.next();
                let Some((k, v)) = next else {
                    panic!("expected Some({}) from iter, received None", x)
                };
                assert_eq!(x, *k);
                assert_eq!(x, *v);
            }
        }
    }

    #[test]
    fn into_iter() {
        let degree = rand::random_range(2..10);
        println!("using degree {}", degree);
        const NUM_VALUES: i32 = 100;
        #[derive(Debug)]
        enum InsertOrder {
            Ordered,
            Reverse,
            Random
        }
        let orders = [InsertOrder::Ordered, InsertOrder::Reverse, InsertOrder::Random];
        for order in orders {
            println!("inserting in {:?} order", order);
            let mut tree = BTree::<i32, i32>::new(degree);
            let to_insert: Vec<i32> = match order {
                InsertOrder::Ordered => (1..=NUM_VALUES).collect(),
                InsertOrder::Reverse => (1..=NUM_VALUES).rev().collect(),
                InsertOrder::Random => {
                    let mut v: Vec<i32> = (1..=NUM_VALUES).collect();
                    v.shuffle(&mut rng());
                    v
                }
            };
            for x in to_insert {
                tree.put(x, x);
            }

            let mut it = tree.into_iter();
            for x in 1..=NUM_VALUES {
                let next = it.next();
                let Some((k, v)) = next else {
                    panic!("expected Some({}) from iter, received None", x)
                };
                assert_eq!(x, k);
                assert_eq!(x, v);
            }
        }
    }
}
