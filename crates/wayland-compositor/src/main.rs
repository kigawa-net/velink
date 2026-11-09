use std::sync::{Arc, Mutex};

fn main() -> std::io::Result<()> {
    let frames = Arc::new(Mutex::new(Vec::new()));
    wayland_compositor::server::serve(None, frames)
}
