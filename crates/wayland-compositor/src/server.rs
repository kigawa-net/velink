//! Minimal Smithay Wayland server with XDG toplevels and SHM frame capture.
//! Frames are copied into owned RGBA buffers on each committed SHM surface.

use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::utils::Serial;
use smithay::wayland::compositor::CompositorClientState as ClientCompositorState;
use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    input::{SeatHandler, SeatState},
};
use smithay::{
    delegate_compositor, delegate_seat, delegate_shm, delegate_xdg_shell,
    reexports::wayland_server::{
        Client, Display, ListeningSocket,
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::{wl_buffer, wl_surface::WlSurface},
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{BufferData, ShmHandler, ShmState, with_buffer_contents},
    },
};
use std::{
    io,
    sync::{Arc, Mutex},
};
use wayland_protocols::xdg::shell::server::xdg_toplevel;

#[derive(Clone, Debug)]
pub struct CpuFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// Raw SHM bytes in the client's advertised format (ARGB8888/XRGB8888).
    pub pixels: Vec<u8>,
}

#[derive(Default)]
struct ClientState {
    compositor: ClientCompositorState,
}
impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

struct State {
    compositor: CompositorState,
    xdg: XdgShellState,
    shm: ShmState,
    frames: Arc<Mutex<Vec<CpuFrame>>>,
    seat_state: SeatState<Self>,
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _: &wl_buffer::WlBuffer) {}
}
impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}
impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|s| {
            s.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
    }
    fn new_popup(&mut self, _: PopupSurface, _: PositionerState) {}
    fn grab(&mut self, _: PopupSurface, _: wl_seat::WlSeat, _: Serial) {}
    fn reposition_request(&mut self, _: PopupSurface, _: PositionerState, _: u32) {}
}
impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("client state")
            .compositor
    }
    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        let Some(buffer) = smithay::wayland::compositor::with_states(surface, |states| {
            states
                .cached_state
                .get::<smithay::wayland::compositor::SurfaceAttributes>()
                .current()
                .buffer
                .as_ref()
                .and_then(|b| match b {
                    smithay::wayland::compositor::BufferAssignment::NewBuffer(buf) => {
                        Some(buf.clone())
                    }
                    smithay::wayland::compositor::BufferAssignment::Removed => None,
                })
        }) else {
            return;
        };
        let Ok(frame) = with_buffer_contents(&buffer, |ptr, len, data| copy_shm(ptr, len, data))
        else {
            return;
        };
        if let Some(frame) = frame {
            self.frames.lock().unwrap().push(frame);
        }
    }
}
impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn focus_changed(&mut self, _: &smithay::input::Seat<Self>, _: Option<&WlSurface>) {}
    fn cursor_image(
        &mut self,
        _: &smithay::input::Seat<Self>,
        _: smithay::input::pointer::CursorImageStatus,
    ) {
    }
}

fn copy_shm(ptr: *const u8, len: usize, data: BufferData) -> Option<CpuFrame> {
    if data.width <= 0 || data.height <= 0 || data.stride <= 0 {
        return None;
    }
    let size = (data.stride as usize).checked_mul(data.height as usize)?;
    if size > len || size > 256 * 1024 * 1024 {
        return None;
    }
    // Smithay exposes the mapped SHM pointer only for the duration of this
    // callback. Copy immediately; never retain or dereference it afterwards.
    let pixels = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
    Some(CpuFrame {
        width: data.width as u32,
        height: data.height as u32,
        stride: data.stride as u32,
        pixels,
    })
}

delegate_compositor!(State);
delegate_seat!(State);
delegate_xdg_shell!(State);
delegate_shm!(State);

/// Serve clients on an auto-named Wayland socket. The returned frame queue
/// receives a CPU copy of every supported SHM buffer committed by a client.
pub fn serve(socket_name: Option<&str>, frames: Arc<Mutex<Vec<CpuFrame>>>) -> io::Result<()> {
    let mut display: Display<State> = Display::new().map_err(io::Error::other)?;
    let dh = display.handle();
    let mut state = State {
        compositor: CompositorState::new::<State>(&dh),
        xdg: XdgShellState::new::<State>(&dh),
        shm: ShmState::new::<State>(&dh, vec![]),
        frames,
        seat_state: SeatState::new(),
    };
    let socket = match socket_name {
        Some(name) => ListeningSocket::bind(name).map_err(io::Error::other)?,
        None => ListeningSocket::bind_auto("velink", 1..32).map_err(io::Error::other)?,
    };
    if let Some(name) = socket.socket_name() {
        eprintln!("WAYLAND_DISPLAY={}", name.to_string_lossy());
    }
    loop {
        if let Some(stream) = socket.accept().map_err(io::Error::other)? {
            display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))
                .map_err(io::Error::other)?;
        }
        display
            .dispatch_clients(&mut state)
            .map_err(io::Error::other)?;
        display.flush_clients().map_err(io::Error::other)?;
        // Avoid spinning while still servicing socket clients promptly.
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
