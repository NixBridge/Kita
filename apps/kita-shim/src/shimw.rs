#![windows_subsystem = "windows"]

mod shim;

fn main() {
    std::process::exit(shim::run());
}
