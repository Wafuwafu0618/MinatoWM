//! Checks whether captured frames contain the mouse cursor and whether the
//! cursor can be hidden and restored. Keep the mouse still while it runs.
//!
//!     cargo run -p minato-wm --example cursor_check

fn main() {
    minato_wm::win32::set_dpi_awareness();
    println!("{}", minato_wm::anim::cursor_report());
}
