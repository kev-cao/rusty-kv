use std::borrow::Borrow;
use std::iter;
use std::mem;

enum Node<K: Ord, V> {
    LNode(LNode<K, V>),
    INode(INode<K,V>),
}

impl<K: Ord, V> Node<K, V> {
    fn put(&mut self, key: K, value: V, degree: usize) -> PutResult<K, V> {
        match self {
            Node::LNode(lnode) => lnode.put(key, value, degree),
            Node::INode(inode) => inode.put(key, value, degree),
        }
    }
}

struct LNode<K: Ord, V> {
    kvs: Vec<(K, V)>
}

impl<K: Ord, V> LNode<K, V> {
    fn should_split(&self, degree:usize) -> bool {
        self.kvs.len() >= 2 * degree
    }

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

struct INode<K: Ord, V> {
    children: Vec<(Node<K, V>, (K, V))>,
    trailing_child: Box<Node<K, V>>
}

impl<K: Ord, V> INode<K, V> {
    fn should_split(&self, degree:usize) -> bool {
        self.children.len() >= 2 * degree
    }

    fn put(&mut self, key: K, value: V, degree: usize) -> PutResult<K, V> {
        let mut insert_idx: usize = 0;
        let put_result = match self.children.binary_search_by(|(_, probe)| probe.0.cmp(&key)) {
            Ok(idx) => {
                self.children[idx].1.1 = value;
                PutResult::Success
            }
            Err(idx) => {
                insert_idx = idx;
                if idx == self.children.len() {
                    self.trailing_child.put(key, value, degree)
                } else {
                    self.children[idx].0.put(key, value, degree)
                }
            }
        };

        let PutResult::Split { promoted_kv, right_sibling  } = put_result else {
            return put_result;
        };

        let mut new_child = (right_sibling, promoted_kv);
        mem::swap(
            &mut new_child.0,
            if insert_idx == self.children.len() {
                &mut self.trailing_child
            } else {
                &mut self.children[insert_idx].0
            },
        );
        self.children.insert(insert_idx, new_child);

        if !self.should_split(degree) {
            return PutResult::Success;
        }

        let (promoted_kv, right_sibling) = self.split();
        PutResult::Split{
            promoted_kv,
            right_sibling: Node::INode(right_sibling),
        }
    }

    fn split(&mut self) -> ((K, V), INode<K, V>){
        let mid = self.children.len() / 2;
        let right_half = self.children.split_off(mid);
        let (mid_prior, mid_kv) = self.children
            .pop()
            .expect("should not happen: node with split with less than 3 elements");
        let right_sibling = INode{
            children: right_half,
            trailing_child: mem::replace(&mut self.trailing_child, Box::new(mid_prior)),
        };
        (mid_kv, right_sibling)
    }
}

pub struct BTree<K: Ord, V> {
    pub degree: usize,
    root: Node<K, V>,
}

impl<K: Ord, V> BTree<K, V> {
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
        let mut curr_node = &self.root;
        loop {
            match curr_node {
                Node::LNode(lnode) => {
                    let res = lnode.kvs.binary_search_by(|(probe, _)| probe.borrow().cmp(key));
                    return if let Ok(idx) = res { Some(&lnode.kvs[idx].1) } else { None };
                },
                Node::INode(inode) => {
                    let res = inode.children.binary_search_by(|(_, probe)| probe.0.borrow().cmp(key));
                    match res {
                        Ok(idx) => return Some(&inode.children[idx].1.1),
                        Err(idx) if idx < inode.children.len() => {
                            curr_node = &inode.children[idx].0
                        }
                        Err(_) => {
                            curr_node = &inode.trailing_child
                        }
                    }
                }
            }
        }
    }

    /// Inserts a key value pair into the tree. If the key already exists,
    /// updates the value mapped to the key.
    pub fn put(&mut self, key: K, value: V) {
        let result = self.root.put(key, value, self.degree);
        let PutResult::Split {promoted_kv, right_sibling} = result else { return };
        // Replace with dummy value.
        let old_root = mem::replace(&mut self.root, Node::LNode(LNode{kvs: Vec::new()}));
        let new_root = Node::INode(INode{
            children: vec![(old_root, promoted_kv)],
            trailing_child: Box::new(right_sibling),
        });
        self.root = new_root;
    }

    pub fn iter(&self) -> BTreeIter<'_, K, V> {
        BTreeIter::new(self)
    }
}

enum PutResult<K: Ord, V> {
    Success,
    Split {
        promoted_kv: (K, V),
        right_sibling: Node<K, V>
    }
}

pub struct BTreeIter<'a, K: Ord, V> {
    cursor: Option<(&'a Node<K, V>, usize)>,
    just_entered_inode: bool,
    parents: Vec<(&'a Node<K, V>, usize)>
}

impl <'a, K: Ord, V> BTreeIter<'a, K, V> {
    fn new(tree: &'a BTree<K, V>) -> Self {
        match &tree.root {
            Node::LNode(lnode) => Self {
                cursor: if lnode.kvs.is_empty() { None } else { Some((&tree.root, 0)) },
                parents: Vec::new(),
                just_entered_inode: false,
            },
            Node::INode(_) => Self {
                cursor: Some((&tree.root, 0)),
                parents: Vec::new(),
                just_entered_inode: true,
            }
        }
    }
}

impl <'a, K: Ord, V> iter::Iterator for BTreeIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        let (mut node, idx) = self.cursor?;
        match node {
            Node::LNode(lnode) => {
                let (k, v) = &lnode.kvs[idx];
                if idx == lnode.kvs.len() - 1 {
                    self.cursor = self.parents.pop();
                } else {
                    self.cursor = Some((node, idx + 1));
                }
                Some((k, v))
            }
            Node::INode(inode) => {
                if self.just_entered_inode {
                    while let Node::INode(inode) = node {
                        self.parents.push((node, 0));
                        node = &inode.children[0].0;
                    }
                    self.cursor = Some((node, 0));
                    self.just_entered_inode = false;
                    self.next()
                } else {
                    let (k, v) = &inode.children[idx].1;
                    let next_node = if idx == inode.children.len() - 1 {
                        &inode.trailing_child
                    } else {
                        self.parents.push((node, idx + 1));
                        &inode.children[idx + 1].0
                    };
                    if let Node::INode(_) = next_node {
                        self.just_entered_inode = true;
                    }
                    self.cursor = Some((next_node, 0));
                    Some((k, v))
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
}
