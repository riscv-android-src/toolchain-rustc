// Checks that lifetimes cannot be interspersed between consts and types.
// revisions: full min
#![feature(const_generics_defaults)]

struct Foo<const N: usize, 'a, T = u32>(&'a (), T);
//~^ Error lifetime parameters must be declared prior to const parameters

struct Bar<const N: usize, T = u32, 'a>(&'a (), T);
//~^ Error lifetime parameters must be declared prior to type parameters

fn main() {}
