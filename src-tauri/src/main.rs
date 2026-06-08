#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--cli") {
        remote_touchpad_lib::run_cli();
    } else {
        remote_touchpad_lib::run()
    }
}
