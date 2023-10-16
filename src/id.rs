use std::fmt::{Debug, Display};
use std::marker::PhantomData;
use std::mem::size_of;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::ops::Range;

#[derive(Copy, Clone)]
pub struct Id<T> {
   _marker: PhantomData<T>,
   pub value: u64
}

impl<T> Eq for Id<T> {}

impl<T> Ord for Id<T> {
   fn cmp(&self, other: &Self) -> std::cmp::Ordering {
      self.value.cmp(&other.value)
   }
}

impl<T> PartialOrd for Id<T> {
   fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
      Some(self.cmp(other))
   }
}

impl<T> Display for Id<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>)
        -> Result<(), std::fmt::Error>
    {
        write!(f, "{}", self.value)
    }
}

impl<T> Debug for Id<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>)
        -> Result<(), std::fmt::Error>
    {
        write!(f, "{}", self.value)
    }
}

pub trait HasLength {
   fn len(&self) -> u64;
}

impl<T> HasLength for Range<Id<T>> {
   fn len(&self) -> u64 {
      self.end.value - self.start.value
   }
}

impl HasLength for Range<u64> {
   fn len(&self) -> u64 {
      self.end - self.start
   }
}

impl<T> PartialEq<Id<T>> for Id<T> {
   fn eq(&self, other: &Id<T>) -> bool {
      self.value.eq(&other.value)
   }
}

impl<T> Add<u64> for Id<T> {
   type Output = Self;

   fn add(self, other: u64) -> Self {
      Id::<T>::from(self.value + other)
   }
}

impl<T> AddAssign<u64> for Id<T> {
   fn add_assign(&mut self, other: u64) {
      self.value += other
   }
}

impl<T> Sub<u64> for Id<T> {
   type Output = Self;

   fn sub(self, other: u64) -> Self {
      Id::<T>::from(self.value - other)
   }
}

impl<T> SubAssign<u64> for Id<T> {
   fn sub_assign(&mut self, other: u64) {
      self.value -= other
   }
}

impl<T> Sub<Id<T>> for Id<T> {
   type Output = u64;

   fn sub(self, other: Id<T>) -> u64 {
      self.value - other.value
   }
}

impl<T> From<u64> for Id<T> {
   fn from(i: u64) -> Self {
      Id::<T> {
         _marker: PhantomData,
         value: i
      }
   }
}

impl<T> From<Id<T>> for u64 {
   fn from(id: Id<T>) -> Self {
      id.value
   }
}

impl<T> Id<T> {
   pub const fn constant(i: u64) -> Self {
      Id::<T> {
         _marker: PhantomData,
         value: i
      }
   }

   pub fn from_offset(offset: u64) -> Id<T> {
      Id {
         _marker: PhantomData,
         value: offset / size_of::<T>() as u64,
      }
   }

   pub fn offset(&self) -> u64 {
      self.value * size_of::<T>() as u64
   }

   pub fn offset_range(&self) -> Range<u64> {
      let size = size_of::<T>() as u64;
      let start = self.value * size;
      let end = start + size;
      start..end
   }
}
