use std::fmt::{Debug, Display};
use std::marker::PhantomData;
use std::ops::{Add, AddAssign, Sub};
use std::ops::Range;

#[derive(Copy, Clone)]
pub struct Id<T> {
   _marker: PhantomData<T>,
   pub value: u64
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
        write!(f, "Id({})", self.value)
    }
}

pub trait HasLength {
   fn len(&self) -> u64;
}

impl<T> HasLength for Range<Id<T>> {
   fn len(&self) -> u64 {
      (self.end.value - self.start.value) as u64
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
}
