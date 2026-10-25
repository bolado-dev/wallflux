// Keep the console window hidden on Windows release builds. wallflux is
// Linux-only today, but this is the one line that is annoying to add later.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    wallflux_gui_lib::run()
}
