use std::marker::PhantomData;
use std::iter::FilterMap;
use std::slice::Iter;

use crate::id::Id;

pub trait Key {
    fn id(self) -> usize;
}

pub struct VecMap<K, V> where K: Key {
    _marker: PhantomData<K>,
    vec: Vec<Option<V>>,
}

impl<K, V> VecMap<K, V> where K: Key {
    pub fn new() -> Self {
        VecMap::<K, V> {
            _marker: PhantomData,
            vec: Vec::new(),
        }
    }

    pub fn with_capacity(size: u8) -> Self {
        VecMap::<K, V> {
            _marker: PhantomData,
            vec: Vec::with_capacity(size as usize),
        }
    }

    pub fn len(&self) -> usize {
        self.vec.len()
    }

    pub fn push(&mut self, value: V) {
        self.vec.push(Some(value))
    }

    pub fn get(&self, index: K) -> Option<&V> {
        match self.vec.get(index.id()) {
            Some(opt) => opt.as_ref(),
            None => None
        }
    }

    pub fn get_mut(&mut self, index: K) -> Option<&mut V> {
        match self.vec.get_mut(index.id()) {
            Some(opt) => opt.as_mut(),
            None => None
        }
    }

    pub fn set(&mut self, index: K, value: V) {
        let id = index.id();
        if id >= self.vec.len() {
            self.vec.resize_with(id + 1, || {None})
        }
        self.vec[id] = Some(value);
    }
}

impl<K, V> Default for VecMap<K, V> where K: Key {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Key for T where T: Into<u8> {
    fn id(self) -> usize {
        self.into() as usize
    }
}

impl<T> Key for Id<T> {
    fn id(self) -> usize {
        self.value as usize
    }
}

#[allow(clippy::type_complexity)]
impl<'v, K, V> IntoIterator for &'v VecMap<K, V> where K: Key {
    type Item = &'v V;
    type IntoIter =
        FilterMap<Iter<'v, Option<V>>, fn(&Option<V>) -> Option<&V>>;

    fn into_iter(self) -> Self::IntoIter {
        self.vec.iter().filter_map(Option::<V>::as_ref)
    }
}
