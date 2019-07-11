// revisions: ast mir
//[mir]compile-flags: -Zborrowck=mir

fn test() {
    let v: isize;
    //[mir]~^ HELP make this binding mutable
    //[mir]~| SUGGESTION mut v
    v = 1; //[ast]~ NOTE first assignment
           //[mir]~^ NOTE first assignment
    println!("v={}", v);
    v = 2; //[ast]~ ERROR cannot assign twice to immutable variable
           //[mir]~^ ERROR cannot assign twice to immutable variable `v`
           //[ast]~| NOTE cannot assign twice to immutable
           //[mir]~| NOTE cannot assign twice to immutable
    println!("v={}", v);
}

fn main() {
}
