use std::marker::PhantomData;
use std::iter::FilterMap;
use std::slice::Iter;

pub struct VecMap<K, V> {
    _marker: PhantomData<K>,
    vec: Vec<Option<V>>,
}

impl<K, V> VecMap<K, V> {
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
}

impl<K, V> VecMap<K, V> where K: Into<u8> {
    pub fn get(&self, index: K) -> Option<&V> {
        match self.vec.get(index.into() as usize) {
            Some(opt) => opt.as_ref(),
            None => None
        }
    }

    pub fn get_mut(&mut self, index: K) -> Option<&mut V> {
        match self.vec.get_mut(index.into() as usize) {
            Some(opt) => opt.as_mut(),
            None => None
        }
    }

    pub fn set(&mut self, index: K, value: V) {
        let index = index.into() as usize;
        if index >= self.vec.len() {
            self.vec.resize_with(index + 1, || {None})
        }
        self.vec[index] = Some(value);
    }
}

impl<'v, K, V> IntoIterator for &'v VecMap<K, V> {
    type Item = &'v V;
    type IntoIter =
        FilterMap<Iter<'v, Option<V>>, fn(&Option<V>) -> Option<&V>>;

    fn into_iter(self) -> Self::IntoIter {
        self.vec.iter().filter_map(Option::<V>::as_ref)
    }
}
