enum Node<K: Ord, V> {
    LNode(LNode<K, V>),
    INode(INode<K,V>),
}

struct LNode<K: Ord, V> {
    kvs: Vec<(K, V)>
}

struct INode<K: Ord, V> {
    children: Vec<(Node<K, V>, (K, V))>,
    trailing_node: Box<Node<K, V>>
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
    ///
    /// # Arguments
    /// * `key` - The key to fetch the value of.
    /// # Returns
    /// An Option wrapping the associated value.
    pub fn get(&self, key: &K) -> Option<&V> {
        let mut curr_node = &self.root;
        loop {
            match curr_node {
                Node::LNode(lnode) => {
                    let res = lnode.kvs.binary_search_by(|&(ref probe, _)| probe.cmp(key));
                    return if let Ok(idx) = res { Some(&lnode.kvs[idx].1) } else { None };
                },
                Node::INode(inode) => {
                    let res = inode.children.binary_search_by(|&(_, ref probe)| probe.0.cmp(key));
                    match res {
                        Ok(idx) => return Some(&inode.children[idx].1.1),
                        Err(idx) if idx < inode.children.len() => {
                            curr_node = &inode.children[idx].0
                        }
                        Err(_) => {
                            curr_node = &inode.trailing_node
                        }
                    }
                }
            }
        }
    }
}
