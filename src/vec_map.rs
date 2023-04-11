use std::iter::FilterMap;
use std::ops::{Index, IndexMut};
use std::marker::PhantomData;
use std::slice::Iter;

use crate::id::Id;

pub trait Key {
    fn id(self) -> usize;

    fn key(id: usize) -> Self;
}

#[derive(Clone)]
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

    pub fn push(&mut self, value: V) -> K {
        self.vec.push(Some(value));
        K::key(self.vec.len())
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

    pub fn last_mut(&mut self) -> Option<&mut V> {
        match self.vec.len() {
            0 => None,
            n => self.get_mut(K::key(n - 1)),
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

impl<T> Key for T where T: From<u8> + Into<u8> {
    fn id(self) -> usize {
        self.into() as usize
    }

    fn key(id: usize) -> T {
        T::from(id.try_into().unwrap())
    }
}

impl<T> Key for Id<T> {
    fn id(self) -> usize {
        self.value as usize
    }

    fn key(id: usize) -> Id<T> {
        Id::<T>::from(id as u64)
    }
}

impl<K, V> Index<K> for VecMap<K, V>
where K: Key
{
    type Output = V;

    fn index(&self, index: K) -> &V {
        self.vec[index.id()].as_ref().unwrap()
    }
}

impl<K, V> IndexMut<K> for VecMap<K, V>
where K: Key
{
    fn index_mut(&mut self, index: K) -> &mut V {
        self.vec[index.id()].as_mut().unwrap()
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
