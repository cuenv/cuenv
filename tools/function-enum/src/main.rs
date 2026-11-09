//! Function enumeration and Rust parameter style checks.

mod enumeration;
mod style_check;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enumeration::run()
}
