use std::borrow::Borrow;
use std::cmp::Ordering;
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
    fn is_full(&self, degree:usize) -> bool {
        self.kvs.len() >= 2 * degree - 1
    }

    fn put(&mut self, key: K, value: V, degree: usize) -> PutResult<K, V> {
        if self.is_full(degree) {
            let (mut promoted_kv, mut right_sibling)= self.split();
            match key.cmp(&promoted_kv.0) {
                Ordering::Less => { self.put(key, value, degree); },
                Ordering::Greater => { right_sibling.put(key, value, degree); },
                Ordering::Equal => { promoted_kv.1 = value; },
            };
            return PutResult::Split{
                promoted_kv,
                right_sibling: Node::LNode(right_sibling)
            };
        }
        match self.kvs.binary_search_by(|&(ref probe, _)| probe.cmp(&key)) {
            Ok(idx) => self.kvs[idx].1 = value,
            Err(idx) => self.kvs.insert(idx, (key, value))
        }
        PutResult::Success
    }

    /// Splits the leaf node at the mid point. This assumes that the leaf node is full.
    ///
    /// # Returns
    /// The extract median KV, and the right sibling.
    fn split(&mut self) -> ((K, V), LNode<K, V>){
        let mid = self.kvs.len() / 2;
        let right_half = self.kvs.split_off(mid+1);
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
    fn is_full(&self, degree:usize) -> bool {
        self.children.len() >= 2 * degree - 1
    }

    fn put(&mut self, key: K, value: V, degree: usize) -> PutResult<K, V> {
        if self.is_full(degree) {
            let (mut promoted_kv, mut right_sibling) = self.split();
            match key.cmp(&promoted_kv.0) {
                Ordering::Less => { self.put(key, value, degree); },
                Ordering::Greater => { right_sibling.put(key, value, degree); },
                Ordering::Equal => { promoted_kv.1 = value },
            }
            return PutResult::Split{
                promoted_kv,
                right_sibling: Node::INode(right_sibling),
            };
        }
        match self.children.binary_search_by(|&(_, ref probe)| probe.0.cmp(&key)) {
            Ok(idx) => self.children[idx].1.1 = value,
            Err(idx) => {
                if idx == self.children.len() {
                    self.trailing_child.put(key, value, degree);
                } else {
                    self.children[idx].0.put(key, value, degree);
                }
            }
        };
        PutResult::Success
    }

    fn split(&mut self) -> ((K, V), INode<K, V>){
        let mid = self.children.len() / 2;
        let right_half = self.children.split_off(mid+1);
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
            degree: degree,
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
                    let res = lnode.kvs.binary_search_by(|&(ref probe, _)| probe.borrow().cmp(key));
                    return if let Ok(idx) = res { Some(&lnode.kvs[idx].1) } else { None };
                },
                Node::INode(inode) => {
                    let res = inode.children.binary_search_by(|&(_, ref probe)| probe.0.borrow().cmp(key));
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
}

enum PutResult<K: Ord, V> {
    Success,
    Split {
        promoted_kv: (K, V),
        right_sibling: Node<K, V>
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
        let mut tree = BTree::<i32, String>::new(DEGREE);

        let mut kvs = vec![
            (1, "one".to_string()),
            (2, "two".to_string()),
            (3, "three".to_string()),
            (4, "four".to_string()),
            (5, "five".to_string()),
            (6, "six".to_string()),
            (7, "seven".to_string()),
            (8, "eight".to_string()),
            (9, "nine".to_string()),
            (10, "ten".to_string()),
        ];
        kvs.shuffle(&mut rng());

        for (k, v) in &kvs {
            tree.put(*k, v.clone());
            assert_eq!(tree.get(k), Some(v));
        }

        for (k, v) in &kvs {
            assert_eq!(tree.get(k), Some(v));
        }

        for k in 11..20 {
            assert_eq!(tree.get(&k), None);
        }
    }
}
