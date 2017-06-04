#![feature(asm)]
#![feature(unique)]
#![feature(const_fn)]
#![feature(lang_items)]
#![no_std]

extern crate rlibc;
extern crate spin;
extern crate volatile;

mod arch;
#[macro_use]
mod macros;

use arch::x86_64::device::vga_console;

#[no_mangle]
pub extern fn kernel_main() {
    //////////// !!! WARNING !!! ////////////
    // THE STACK IS LARGER NOW, BUT        //
    // WE STILL HAVE NO GUARD PAGE         //
    /////////////////////////////////////////

    vga_console::WRITER.lock().clear_screen();
    println!("Hello world!");


    loop {}
}

#[lang = "eh_personality"]
#[no_mangle]
pub extern fn eh_personality() {}
#[lang = "panic_fmt"]
#[no_mangle]
pub extern fn panic_fmt() -> ! {loop{}}

#[allow(non_snake_case)]
#[no_mangle]
pub extern "C" fn _Unwind_Resume() -> ! {
    // we should hlt here
    loop {}
}
