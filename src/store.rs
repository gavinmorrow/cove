use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;
use std::vec;

use async_trait::async_trait;

pub trait Msg {
    type Id: Clone + Debug + Hash + Eq + Ord;
    fn id(&self) -> Self::Id;
    fn parent(&self) -> Option<Self::Id>;
    fn seen(&self) -> bool;

    fn last_possible_id() -> Self::Id;
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub struct Path<I>(Vec<I>);

impl<I> Path<I> {
    pub fn new(segments: Vec<I>) -> Self {
        assert!(!segments.is_empty(), "segments must not be empty");
        Self(segments)
    }

    pub fn parent_segments(&self) -> impl Iterator<Item = &I> {
        self.0.iter().take(self.0.len() - 1)
    }

    pub fn push(&mut self, segment: I) {
        self.0.push(segment)
    }

    pub fn first(&self) -> &I {
        self.0.first().expect("path is not empty")
    }
}

impl<I> IntoIterator for Path<I> {
    type Item = I;
    type IntoIter = vec::IntoIter<I>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

pub struct Tree<M: Msg> {
    root: M::Id,
    msgs: HashMap<M::Id, M>,
    children: HashMap<M::Id, Vec<M::Id>>,
}

impl<M: Msg> Tree<M> {
    pub fn new(root: M::Id, msgs: Vec<M>) -> Self {
        let msgs: HashMap<M::Id, M> = msgs.into_iter().map(|m| (m.id(), m)).collect();

        let mut children: HashMap<M::Id, Vec<M::Id>> = HashMap::new();
        for msg in msgs.values() {
            children.entry(msg.id()).or_default();
            if let Some(parent) = msg.parent() {
                children.entry(parent).or_default().push(msg.id());
            }
        }

        for list in children.values_mut() {
            list.sort_unstable();
        }

        Self { root, msgs, children }
    }

    pub fn len(&self) -> usize {
        self.msgs.len()
    }

    pub fn root(&self) -> &M::Id {
        &self.root
    }

    pub fn msg(&self, id: &M::Id) -> Option<&M> {
        self.msgs.get(id)
    }

    pub fn parent(&self, id: &M::Id) -> Option<M::Id> {
        self.msg(id).and_then(|m| m.parent())
    }

    pub fn children(&self, id: &M::Id) -> Option<&[M::Id]> {
        self.children.get(id).map(|c| c as &[M::Id])
    }

    pub fn subtree_size(&self, id: &M::Id) -> usize {
        let children = self.children(id).unwrap_or_default();
        let mut result = children.len();
        for child in children {
            result += self.subtree_size(child);
        }
        result
    }

    pub fn siblings(&self, id: &M::Id) -> Option<&[M::Id]> {
        if let Some(parent) = self.parent(id) {
            self.children(&parent)
        } else {
            None
        }
    }

    pub fn prev_sibling(&self, id: &M::Id) -> Option<M::Id> {
        let siblings = self.siblings(id)?;
        siblings
            .iter()
            .zip(siblings.iter().skip(1))
            .find(|(_, s)| *s == id)
            .map(|(s, _)| s.clone())
    }

    pub fn next_sibling(&self, id: &M::Id) -> Option<M::Id> {
        let siblings = self.siblings(id)?;
        siblings
            .iter()
            .zip(siblings.iter().skip(1))
            .find(|(s, _)| *s == id)
            .map(|(_, s)| s.clone())
    }
}

#[async_trait]
pub trait MsgStore<M: Msg> {
    async fn path(&self, id: &M::Id) -> Path<M::Id>;
    async fn msg(&self, id: &M::Id) -> Option<M>;
    async fn tree(&self, tree_id: &M::Id) -> Tree<M>;
    async fn first_tree_id(&self) -> Option<M::Id>;
    async fn last_tree_id(&self) -> Option<M::Id>;
    async fn prev_tree_id(&self, tree_id: &M::Id) -> Option<M::Id>;
    async fn next_tree_id(&self, tree_id: &M::Id) -> Option<M::Id>;
    async fn oldest_msg_id(&self) -> Option<M::Id>;
    async fn newest_msg_id(&self) -> Option<M::Id>;
    async fn older_msg_id(&self, id: &M::Id) -> Option<M::Id>;
    async fn newer_msg_id(&self, id: &M::Id) -> Option<M::Id>;
    async fn oldest_unseen_msg_id(&self) -> Option<M::Id>;
    async fn newest_unseen_msg_id(&self) -> Option<M::Id>;
    async fn older_unseen_msg_id(&self, id: &M::Id) -> Option<M::Id>;
    async fn newer_unseen_msg_id(&self, id: &M::Id) -> Option<M::Id>;
    async fn unseen_msgs_count(&self) -> usize;
    async fn set_seen(&self, id: &M::Id, seen: bool);
    async fn set_older_seen(&self, id: &M::Id, seen: bool);
}
