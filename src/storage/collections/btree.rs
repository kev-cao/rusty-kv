use super::KVIterator;
use std::{borrow::Borrow, collections::VecDeque, fmt::Debug, iter::Iterator, mem, vec};

#[derive(Debug)]
enum Node<K: Ord, V> {
    LNode(LNode<K, V>),
    INode(INode<K, V>),
}

impl<K: Ord, V> Node<K, V> {
    /// Fetches the value associated with the provided key, if it exists.
    fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self {
            Node::LNode(lnode) => lnode.get(key),
            Node::INode(inode) => inode.get(key),
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

impl<K: Ord, V> Default for Node<K, V> {
    fn default() -> Self {
        Self::LNode(LNode { kvs: Vec::new() })
    }
}

#[derive(Default, Debug)]
struct LNode<K: Ord, V> {
    kvs: Vec<(K, V)>,
}

impl<K: Ord, V> LNode<K, V> {
    /// Returns true if the leaf node has reached the maximum capacity and
    /// needs to be split.
    fn should_split(&self, degree: usize) -> bool {
        self.kvs.len() >= 2 * degree
    }

    /// Fetches the value associated with the provided key, if it exists.
    fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.kvs
            .binary_search_by(|(probe, _)| probe.borrow().cmp(key))
            .ok()
            .map(|idx| &self.kvs[idx].1)
    }

    /// Inserts a key/value pair into the node.
    fn put(&mut self, key: K, value: V, degree: usize) -> PutResult<K, V> {
        match self.kvs.binary_search_by(|(probe, _)| probe.cmp(&key)) {
            Ok(idx) => self.kvs[idx].1 = value,
            Err(idx) => self.kvs.insert(idx, (key, value)),
        }
        if !self.should_split(degree) {
            return PutResult::Success;
        }
        let (promoted_kv, right_sibling) = self.split();
        PutResult::Split {
            promoted_kv,
            right_sibling: Node::LNode(right_sibling),
        }
    }

    /// Splits the leaf node at the mid point. This assumes that the leaf node
    /// needs to be split.
    ///
    /// # Returns
    /// The extracted median KV, and the right sibling.
    fn split(&mut self) -> ((K, V), LNode<K, V>) {
        let mid = self.kvs.len() / 2;
        let right_half = self.kvs.split_off(mid);
        let mid_kv = self
            .kvs
            .pop()
            .expect("should not happen: node with split with less than 3 elements");
        (mid_kv, LNode { kvs: right_half })
    }
}

#[derive(Default, Debug)]
struct INode<K: Ord, V> {
    /// Always one more than the number of KVs.
    children: Vec<Node<K, V>>,
    kvs: Vec<(K, V)>,
}

impl<K: Ord, V> INode<K, V> {
    /// Returns true if the internal node has reached the maximum capacity and
    /// needs to be split.
    fn should_split(&self, degree: usize) -> bool {
        self.kvs.len() >= 2 * degree
    }

    /// Fetches the value associated with the provided key, if it exists.
    fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self
            .kvs
            .binary_search_by(|(probe, _)| probe.borrow().cmp(key))
        {
            Ok(idx) => Some(&self.kvs[idx].1),
            Err(idx) => self.children[idx].get(key),
        }
    }

    /// Inserts a key/value pair into the node.
    fn put(&mut self, key: K, value: V, degree: usize) -> PutResult<K, V> {
        let Err(ins_idx) = self.kvs.binary_search_by(|(probe, _)| probe.cmp(&key)) else {
            return PutResult::Success;
        };

        let put_result = self.children[ins_idx].put(key, value, degree);
        let PutResult::Split {
            promoted_kv,
            right_sibling,
        } = put_result
        else {
            return put_result;
        };

        self.kvs.insert(ins_idx, promoted_kv);
        self.children.insert(ins_idx + 1, right_sibling);

        if !self.should_split(degree) {
            return PutResult::Success;
        }

        let (promoted_kv, right_sibling) = self.split();
        PutResult::Split {
            promoted_kv,
            right_sibling: Node::INode(right_sibling),
        }
    }

    /// Splits the internal node at the mid point. This assumes that the
    /// internal node needs to be split.
    ///
    /// # Returns
    /// The extracted median KV, and the right sibling.
    fn split(&mut self) -> ((K, V), INode<K, V>) {
        let mid = self.kvs.len() / 2;
        let right_kvs = self.kvs.split_off(mid);
        let right_children = self.children.split_off(mid);
        let promoted_kv = self
            .kvs
            .pop()
            .expect("should not happen; split occurred with less than 3 elements");
        let right_sibling = INode {
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
        right_sibling: Node<K, V>,
    },
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
            root: Node::LNode(LNode { kvs: Vec::new() }),
        }
    }

    /// Returns true if the tree contains no elements.
    pub fn is_empty(&self) -> bool {
        match &self.root {
            Node::LNode(lnode) => lnode.kvs.is_empty(),
            Node::INode(..) => false,
        }
    }

    /// Fetches the value associated with the key in the tree, if it exists.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.root.get(key)
    }

    /// Inserts a key value pair into the tree. If the key already exists,
    /// updates the value mapped to the key.
    pub fn put(&mut self, key: K, value: V) {
        let result = self.root.put(key, value, self.degree);
        let PutResult::Split {
            promoted_kv,
            right_sibling,
        } = result
        else {
            return;
        };
        // Replace with dummy value.
        let old_root = mem::replace(&mut self.root, Node::LNode(LNode { kvs: Vec::new() }));
        let new_root = Node::INode(INode {
            kvs: vec![promoted_kv],
            children: vec![old_root, right_sibling],
        });
        self.root = new_root;
    }

    pub fn kv_iter(&self) -> impl KVIterator<Key = K, Value = V> {
        BTreeIter::new(self)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        BTreeIter::new(self)
    }
}

enum PrintFrame<'a, K: Ord + Debug, V: Debug> {
    Newline,
    Node(&'a Node<K, V>),
}

impl<'a, K: Ord + Debug, V: Debug> From<&'a Node<K, V>> for PrintFrame<'a, K, V> {
    fn from(node: &'a Node<K, V>) -> Self {
        Self::Node(node)
    }
}

impl<K, V> Debug for BTree<K, V>
where
    K: Ord + Debug,
    V: Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return write!(f, "BTree {{}}");
        }
        let mut queue: VecDeque<PrintFrame<K, V>> = VecDeque::new();
        queue.push_back(PrintFrame::from(&self.root));
        queue.push_back(PrintFrame::Newline);

        while let Some(frame) = queue.pop_front() {
            match frame {
                PrintFrame::Newline => {
                    writeln!(f)?;
                    if !queue.is_empty() {
                        queue.push_back(PrintFrame::Newline);
                    }
                }
                PrintFrame::Node(node) => match node {
                    Node::LNode(lnode) => {
                        write!(f, "LNode {{ kvs: {:?} }}", lnode.kvs)?;
                    }
                    Node::INode(inode) => {
                        write!(f, "INode {{ kvs: {:?} }}", inode.kvs)?;
                        for child in &inode.children {
                            queue.push_back(PrintFrame::from(child));
                        }
                        write!(f, "] }}")?;
                    }
                },
            };
        }
        Ok(())
    }
}

// An IterFrame represents a frame in the stack of the BTree iterator. It can
// either be a leaf node or an internal node, along with a cursor indicating the
// current position in the node. For an INode, the cursor is applied to both the
// kvs and the children, meaning that 0 is the leftmost child, 1 is the first
// kv, 2 is the second child, 3 is the second kv, etc. The cursor can be a
// maximum of kv.len() + children.len(), in which case the inode is considered
// exhausted.
#[derive(Debug)]
enum IterFrame<'a, K: Ord, V> {
    LNode(&'a LNode<K, V>, usize),
    INode(&'a INode<K, V>, usize),
}

impl<'a, K: Ord, V> From<&'a Node<K, V>> for IterFrame<'a, K, V> {
    fn from(node: &'a Node<K, V>) -> Self {
        match node {
            Node::LNode(lnode) => Self::LNode(lnode, 0),
            Node::INode(inode) => Self::INode(inode, 0),
        }
    }
}

#[derive(Debug)]
pub struct BTreeIter<'a, K: Ord, V> {
    root: &'a Node<K, V>,
    stack: Vec<IterFrame<'a, K, V>>,
    /// is_reset is set when the stack is empty and the cursor is set to the
    /// beginning of the tree. This is used to ensure that the first call to
    /// next() points the cursor to the first KV in the tree.
    is_reset: bool,
}

impl<'a, K: Ord, V> BTreeIter<'a, K, V> {
    /// Constructs a new iterator from a BTree.
    fn new(btree: &'a BTree<K, V>) -> Self {
        Self {
            root: &btree.root,
            stack: vec![],
            is_reset: true,
        }
    }
}

impl<'a, K: Ord, V> BTreeIter<'a, K, V> {
    pub fn curr_kv(&self) -> Option<&'a (K, V)> {
        let frame = self.stack.last()?;
        Some(match frame {
            IterFrame::LNode(lnode, cursor) => &lnode.kvs[*cursor],
            IterFrame::INode(inode, cursor) => {
                if cursor % 2 == 0 {
                    let child = &inode.children[*cursor / 2];
                    match child {
                        Node::LNode(lnode) => &lnode.kvs[0],
                        Node::INode(inode) => &inode.kvs[0],
                    }
                } else {
                    &inode.kvs[cursor / 2]
                }
            }
        })
    }

    pub fn reset(&mut self) {
        self.stack.clear();
        self.traverse_leftmost_leaf(self.root);
        self.is_reset = false;
    }

    /// Traverses the leftmost path of the tree from the provided node, pushing
    /// each node onto the stack.
    fn traverse_leftmost_leaf(&mut self, node: &'a Node<K, V>) {
        let mut curr: IterFrame<K, V> = node.into();
        while let IterFrame::INode(inode, ref mut cursor) = curr {
            *cursor += 1;
            self.stack.push(curr);
            curr = (&inode.children[0]).into();
        }
        self.stack.push(curr);
    }

    /// Pops any exhausted inodes from the top of the stack until the top of the
    /// stack is either a leaf node or an inode that has not been exhausted.
    fn truncate_exhausted_inodes(&mut self) {
        while let Some(IterFrame::INode(inode, cursor)) = self.stack.last()
            && *cursor >= inode.kvs.len() + inode.children.len()
        {
            self.stack.pop();
        }
    }
}

impl<'a, K: Ord, V> KVIterator for BTreeIter<'a, K, V> {
    type Key = K;
    type Value = V;
    type Error = ();

    fn key(&self) -> Result<Option<&'a Self::Key>, Self::Error> {
        Ok(self.curr_kv().map(|kv| &kv.0))
    }

    fn value(&self) -> Result<Option<&'a Self::Value>, Self::Error> {
        Ok(self.curr_kv().map(|kv| &kv.1))
    }

    fn next(&mut self) -> Result<bool, Self::Error> {
        if self.is_reset {
            self.reset();
            return Ok(!self.stack.is_empty());
        }

        let Some(mut frame) = self.stack.pop() else {
            return Ok(false);
        };
        match frame {
            IterFrame::LNode(lnode, ref mut cursor) => {
                *cursor += 1;
                if *cursor < lnode.kvs.len() {
                    self.stack.push(frame);
                    return Ok(true);
                }
                self.truncate_exhausted_inodes();
            }
            IterFrame::INode(inode, ref mut cursor) => {
                if *cursor >= inode.kvs.len() + inode.children.len() {
                    self.truncate_exhausted_inodes();
                    return Ok(!self.stack.is_empty());
                }
                *cursor += 1;
                if *cursor % 2 == 0 {
                    let child = &inode.children[*cursor / 2];
                    *cursor += 1;
                    self.stack.push(frame);
                    self.traverse_leftmost_leaf(child);
                } else {
                    self.stack.push(frame);
                }
            }
        }
        Ok(!self.stack.is_empty())
    }

    fn seek_ge<Q>(&mut self, key: &Q) -> Result<bool, Self::Error>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.is_reset = false;
        self.stack.clear();
        self.stack.push(self.root.into());

        loop {
            let Some(mut frame) = self.stack.pop() else {
                return Ok(false);
            };

            match frame {
                IterFrame::LNode(lnode, ref mut cursor) => {
                    let idx = match lnode
                        .kvs
                        .binary_search_by(|(probe, _)| probe.borrow().cmp(key))
                    {
                        Ok(idx) | Err(idx) => idx,
                    };
                    if idx < lnode.kvs.len() {
                        *cursor = idx;
                        self.stack.push(frame);
                        return Ok(true);
                    } else {
                        self.truncate_exhausted_inodes();
                        return Ok(!self.stack.is_empty());
                    }
                }
                IterFrame::INode(inode, ref mut cursor) => {
                    match inode
                        .kvs
                        .binary_search_by(|(probe, _)| probe.borrow().cmp(key))
                    {
                        Ok(idx) => {
                            *cursor = idx * 2 + 1;
                            self.stack.push(frame);
                            return Ok(true);
                        }
                        Err(idx) => {
                            *cursor = idx * 2 + 1;
                            self.stack.push(frame);
                            self.stack.push((&inode.children[idx]).into());
                        }
                    };
                }
            };
        }
    }

    fn seek_lt<Q>(&mut self, key: &Q) -> Result<bool, Self::Error>
    where
        Self::Key: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.is_reset = false;
        self.stack.clear();
        self.stack.push(self.root.into());

        loop {
            let Some(mut frame) = self.stack.pop() else {
                return Ok(false);
            };

            match frame {
                IterFrame::LNode(lnode, ref mut cursor) => {
                    let idx = match lnode
                        .kvs
                        .binary_search_by(|(probe, _)| probe.borrow().cmp(key))
                    {
                        Ok(idx) | Err(idx) => idx,
                    };
                    if idx > 0 {
                        *cursor = idx - 1;
                        self.stack.push(frame);
                        return Ok(true);
                    }
                    loop {
                        let Some(mut frame) = self.stack.pop() else {
                            return Ok(false);
                        };
                        match frame {
                            IterFrame::LNode(..) => return Ok(false),
                            IterFrame::INode(_, ref mut cursor) => {
                                if *cursor <= 1 {
                                    continue;
                                }
                                *cursor -= 2;
                                self.stack.push(frame);
                                return Ok(true);
                            }
                        }
                    }
                }
                IterFrame::INode(inode, ref mut cursor) => {
                    let idx = match inode
                        .kvs
                        .binary_search_by(|(probe, _)| probe.borrow().cmp(key))
                    {
                        Ok(idx) | Err(idx) => idx,
                    };
                    *cursor = idx * 2 + 1;
                    self.stack.push(frame);
                    self.stack.push((&inode.children[idx]).into());
                }
            };
        }
    }
}

impl<'a, K: Ord, V> Iterator for BTreeIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        if !KVIterator::next(self).unwrap_or(false) {
            return None;
        }
        let kv = self.curr_kv()?;
        Some((&kv.0, &kv.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::seq::SliceRandom;
    use rand::{Rng, rng};

    #[derive(Debug)]
    enum InsertOrder {
        Ordered,
        Reverse,
        Random,
    }

    #[test]
    fn put_get_roundtrip() {
        const DEGREE: usize = 4;
        let num_values: i32 = rand::random_range(10..100);
        let mut tree = BTree::<i32, String>::new(DEGREE);
        let mut keys: Vec<i32> = (1..=num_values).collect();
        keys.shuffle(&mut rng());

        for k in &keys {
            tree.put(*k, k.to_string());
            assert_eq!(tree.get(k), Some(&k.to_string()));
        }
        println!("tree: {:?}", tree);

        for k in &keys {
            assert_eq!(tree.get(k), Some(&k.to_string()));
        }

        assert_eq!(tree.get(&(num_values + 1)), None);
    }

    #[test]
    fn kv_iter_next() {
        let degree = rand::random_range(2..10);
        println!("using degree {}", degree);
        let num_values: i32 = rand::random_range(10..100);
        let orders = [
            InsertOrder::Ordered,
            InsertOrder::Reverse,
            InsertOrder::Random,
        ];
        for order in orders {
            println!("inserting in {:?} order", order);
            let mut tree = BTree::<i32, i32>::new(degree);
            let to_insert: Vec<i32> = match order {
                InsertOrder::Ordered => (1..=num_values).collect(),
                InsertOrder::Reverse => (1..=num_values).rev().collect(),
                InsertOrder::Random => {
                    let mut v: Vec<i32> = (1..=num_values).collect();
                    v.shuffle(&mut rng());
                    v
                }
            };
            for x in to_insert {
                tree.put(x, x);
            }
            println!("tree: {:?}", tree);

            let mut it = tree.kv_iter();
            for x in 1..=num_values {
                assert!(
                    it.next()
                        .expect("expected Ok(true) from iter, received Err or false")
                );
                let Ok(Some(k)) = it.key() else {
                    panic!("expected Ok(Some(k)) from iter, received Err or None");
                };
                let Ok(Some(v)) = it.value() else {
                    panic!("expected Ok(Some(v)) from iter, received Err or None");
                };
                assert_eq!(*k, x, "wrong key");
                assert_eq!(*v, x, "wrong value");
            }
        }
    }

    #[test]
    fn iter_next() {
        let degree = rand::random_range(2..10);
        println!("using degree {}", degree);
        let num_values: i32 = rand::random_range(10..100);
        let orders = [
            InsertOrder::Ordered,
            InsertOrder::Reverse,
            InsertOrder::Random,
        ];
        for order in orders {
            println!("inserting in {:?} order", order);
            let mut tree = BTree::<i32, i32>::new(degree);
            let to_insert: Vec<i32> = match order {
                InsertOrder::Ordered => (1..=num_values).collect(),
                InsertOrder::Reverse => (1..=num_values).rev().collect(),
                InsertOrder::Random => {
                    let mut v: Vec<i32> = (1..=num_values).collect();
                    v.shuffle(&mut rng());
                    v
                }
            };
            for x in to_insert {
                tree.put(x, x);
            }
            println!("tree: {:?}", tree);

            let mut it = tree.iter();
            for x in 1..=num_values {
                let (k, v) = it
                    .next()
                    .expect("expected Some((k, v)) from iter, received None");
                assert_eq!(*k, x, "wrong key");
                assert_eq!(*v, x, "wrong value");
            }
        }
    }

    #[test]
    fn seek_ge() {
        let degree = rand::random_range(2..10);
        println!("using degree {}", degree);
        let num_values: i32 = rand::random_range(10..100);

        let orders = [
            InsertOrder::Ordered,
            InsertOrder::Reverse,
            InsertOrder::Random,
        ];
        for order in orders {
            println!("inserting in {:?} order", order);
            let mut tree = BTree::<i32, i32>::new(degree);
            let to_insert: Vec<i32> = match order {
                InsertOrder::Ordered => (1..=num_values * 2).step_by(2).collect(),
                InsertOrder::Reverse => (1..=num_values * 2 - 1).rev().step_by(2).collect(),
                InsertOrder::Random => {
                    let mut v: Vec<i32> = (1..=num_values * 2).step_by(2).collect();
                    v.shuffle(&mut rng());
                    v
                }
            };
            for x in to_insert {
                tree.put(x, x);
            }
            println!("tree: {:?}", tree);

            let start = rng().random_range(1..num_values * 2 - 1);
            let mut start_odd = start;
            let mut start_even = start + 1;
            if start_even % 2 != 0 {
                (start_odd, start_even) = (start_even, start_odd);
            }

            println!("start_odd = {}, start_even = {}", start_odd, start_even);

            let mut it = tree.kv_iter();
            assert!(
                it.seek_ge(&start_odd)
                    .expect("expected Ok(true) from iter, received Err or false")
            );
            let Ok(Some(k)) = it.key() else {
                panic!("expected Ok(Some(k)) from iter, received Err or None");
            };
            let Ok(Some(v)) = it.value() else {
                panic!("expected Ok(Some(v)) from iter, received Err or None");
            };
            assert_eq!(*k, start_odd, "wrong key");
            assert_eq!(*v, start_odd, "wrong value");

            for x in (start_odd + 2..=num_values * 2).step_by(2) {
                assert!(
                    it.next()
                        .expect("expected Ok(true) from iter, received Err or false")
                );
                let Ok(Some(k)) = it.key() else {
                    panic!("expected Ok(Some(k)) from iter, received Err or None");
                };
                let Ok(Some(v)) = it.value() else {
                    panic!("expected Ok(Some(v)) from iter, received Err or None");
                };
                assert_eq!(*k, x, "wrong key");
                assert_eq!(*v, x, "wrong value");
            }

            assert!(
                it.seek_ge(&start_even)
                    .expect("expected Ok(true) from iter, received Err or false")
            );
            let Ok(Some(k)) = it.key() else {
                panic!("expected Ok(Some(k)) from iter, received Err or None");
            };
            let Ok(Some(v)) = it.value() else {
                panic!("expected Ok(Some(v)) from iter, received Err or None");
            };
            assert_eq!(*k, start_even + 1);
            assert_eq!(*v, start_even + 1);

            for x in (start_even + 3..=num_values * 2).step_by(2) {
                assert!(
                    it.next()
                        .expect("expected Ok(true) from iter, received Err or false")
                );
                let Ok(Some(k)) = it.key() else {
                    panic!("expected Ok(Some(k)) from iter, received Err or None");
                };
                let Ok(Some(v)) = it.value() else {
                    panic!("expected Ok(Some(v)) from iter, received Err or None");
                };
                assert_eq!(*k, x, "wrong key");
                assert_eq!(*v, x, "wrong value");
            }
        }
    }

    #[test]
    fn seek_lt() {
        let degree = rand::random_range(2..10);
        println!("using degree {}", degree);
        let num_values: i32 = rand::random_range(10..100);

        let orders = [
            InsertOrder::Ordered,
            InsertOrder::Reverse,
            InsertOrder::Random,
        ];
        for order in orders {
            println!("inserting in {:?} order", order);
            let mut tree = BTree::<i32, i32>::new(degree);
            let to_insert: Vec<i32> = match order {
                InsertOrder::Ordered => (1..=num_values * 2).step_by(2).collect(),
                InsertOrder::Reverse => (1..=num_values * 2 - 1).rev().step_by(2).collect(),
                InsertOrder::Random => {
                    let mut v: Vec<i32> = (1..=num_values * 2).step_by(2).collect();
                    v.shuffle(&mut rng());
                    v
                }
            };
            for x in to_insert {
                tree.put(x, x);
            }
            println!("tree: {:?}", tree);

            let start = rng().random_range(2..num_values * 2);
            let mut start_odd = start;
            let mut start_even = start + 1;
            if start_even % 2 != 0 {
                (start_odd, start_even) = (start_even, start_odd);
            }

            println!("start_odd = {}, start_even = {}", start_odd, start_even);

            let mut it = tree.kv_iter();
            assert!(
                it.seek_lt(&start_odd)
                    .expect("expected Ok(true) from iter, received Err or false")
            );
            let Ok(Some(k)) = it.key() else {
                panic!("expected Ok(Some(k)) from iter, received Err or None");
            };
            let Ok(Some(v)) = it.value() else {
                panic!("expected Ok(Some(v)) from iter, received Err or None");
            };
            assert_eq!(*k, start_odd - 2, "wrong key");
            assert_eq!(*v, start_odd - 2, "wrong value");

            for x in (start_odd..=num_values * 2).step_by(2) {
                assert!(
                    it.next()
                        .expect("expected Some(true) from iter, received None or false")
                );
                let Ok(Some(k)) = it.key() else {
                    panic!("expected Ok(Some(k)) from iter, received Err or None");
                };
                let Ok(Some(v)) = it.value() else {
                    panic!("expected Ok(Some(v)) from iter, received Err or None");
                };
                assert_eq!(*k, x, "wrong key");
                assert_eq!(*v, x, "wrong value");
            }

            assert!(
                it.seek_lt(&start_even)
                    .expect("expected Ok(true) from iter, received Err or false")
            );
            let Ok(Some(k)) = it.key() else {
                panic!("expected Ok(Some(k)) from iter, received Err or None");
            };
            let Ok(Some(v)) = it.value() else {
                panic!("expected Ok(Some(v)) from iter, received Err or None");
            };
            assert_eq!(*k, start_even - 1);
            assert_eq!(*v, start_even - 1);

            for x in (start_even + 1..=num_values * 2).step_by(2) {
                assert!(
                    it.next()
                        .expect("expected Some(true) from iter, received None or false")
                );
                let Ok(Some(k)) = it.key() else {
                    panic!("expected Ok(Some(k)) from iter, received Err or None");
                };
                let Ok(Some(v)) = it.value() else {
                    panic!("expected Ok(Some(v)) from iter, received Err or None");
                };
                assert_eq!(*k, x, "wrong key");
                assert_eq!(*v, x, "wrong value");
            }
        }
    }
}
