//! RFB 3.8 viewer: raw 32-bit pixels over binary WebSocket messages.
use futures_util::{SinkExt, StreamExt};
use minifb::{Key, MouseButton, MouseMode, Window, WindowOptions};
use std::{
    collections::HashMap,
    error::Error,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
};
use tokio_tungstenite::tungstenite::Message;
use velink_core::remote::{RemoteSession, VncConnectionState};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const MAX_PIXELS: usize = 16_777_216;
const MAX_TEXT: usize = 1024 * 1024;

#[derive(Default)]
struct Frame {
    width: u16,
    height: u16,
    pixels: Vec<u32>,
}
struct Shared {
    session: RemoteSession,
    frame: Frame,
}

type State = Arc<Mutex<Shared>>;

async fn bridge(url: &str, stream: tokio::io::DuplexStream) -> Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut sink, mut source) = ws.split();
    let (mut read, mut write) = tokio::io::split(stream);
    let incoming = async {
        while let Some(message) = source.next().await {
            match message? {
                Message::Binary(data) => write.write_all(&data).await?,
                Message::Close(_) => break,
                Message::Text(_) => return Err("expected binary VNC data".into()),
                _ => {}
            }
        }
        Result::Ok(())
    };
    let outgoing = async {
        let mut buffer = vec![0; 65536];
        loop {
            let n = read.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            sink.send(Message::Binary(buffer[..n].to_vec().into()))
                .await?;
        }
        Result::Ok(())
    };
    tokio::select! { result = incoming => result, result = outgoing => result }
}

async fn bounded_text<R: AsyncRead + Unpin>(read: &mut R) -> Result<Vec<u8>> {
    let length = read.read_u32().await? as usize;
    if length > MAX_TEXT {
        return Err("RFB text exceeds 1 MiB".into());
    }
    let mut text = vec![0; length];
    read.read_exact(&mut text).await?;
    Ok(text)
}

async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<Frame> {
    let mut version = [0; 12];
    stream.read_exact(&mut version).await?;
    if &version != b"RFB 003.008\n" {
        return Err("server must support RFB 3.8".into());
    }
    stream.write_all(b"RFB 003.008\n").await?;
    let count = stream.read_u8().await?;
    if count == 0 {
        return Err(String::from_utf8_lossy(&bounded_text(stream).await?)
            .into_owned()
            .into());
    }
    let mut security = vec![0; count as usize];
    stream.read_exact(&mut security).await?;
    if !security.contains(&1) {
        return Err("server does not offer SecurityTypes=None".into());
    }
    stream.write_all(&[1]).await?;
    if stream.read_u32().await? != 0 {
        return Err(String::from_utf8_lossy(&bounded_text(stream).await?)
            .into_owned()
            .into());
    }
    stream.write_all(&[1]).await?; // shared desktop; gateway enforces one viewer
    let width = stream.read_u16().await?;
    let height = stream.read_u16().await?;
    let count = usize::from(width) * usize::from(height);
    if count == 0 || count > MAX_PIXELS {
        return Err("invalid framebuffer dimensions".into());
    }
    let mut format = [0; 16];
    stream.read_exact(&mut format).await?;
    let _name = bounded_text(stream).await?;
    // SetPixelFormat: 32 bpp, 24-bit depth, little endian, true colour, RGB shifts 16/8/0.
    stream
        .write_all(&[
            0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0,
        ])
        .await?;
    // SetEncodings: Raw only. No resize pseudo-encoding is advertised.
    stream.write_all(&[2, 0, 0, 1, 0, 0, 0, 0]).await?;
    stream
        .write_all(&update_request(width, height, false))
        .await?;
    Ok(Frame {
        width,
        height,
        pixels: vec![0; count],
    })
}

fn update_request(width: u16, height: u16, incremental: bool) -> Vec<u8> {
    let mut message = vec![3, u8::from(incremental), 0, 0, 0, 0];
    message.extend(width.to_be_bytes());
    message.extend(height.to_be_bytes());
    message
}

async fn read_updates<R: AsyncRead + Unpin>(
    mut read: R,
    state: State,
    requests: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    loop {
        match read.read_u8().await? {
            0 => {
                let _padding = read.read_u8().await?;
                let rectangles = read.read_u16().await?;
                for _ in 0..rectangles {
                    let x = read.read_u16().await? as usize;
                    let y = read.read_u16().await? as usize;
                    let width = read.read_u16().await? as usize;
                    let height = read.read_u16().await? as usize;
                    if read.read_i32().await? != 0 {
                        return Err("server sent an unrequested encoding".into());
                    }
                    {
                        let shared = state.lock().unwrap();
                        if x + width > shared.frame.width as usize
                            || y + height > shared.frame.height as usize
                        {
                            return Err("rectangle exceeds framebuffer".into());
                        }
                    }
                    let mut data = vec![0; width * height * 4];
                    read.read_exact(&mut data).await?;
                    let mut shared = state.lock().unwrap();
                    let stride = shared.frame.width as usize;
                    for row in 0..height {
                        for col in 0..width {
                            let offset = (row * width + col) * 4;
                            shared.frame.pixels[(y + row) * stride + x + col] =
                                u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
                                    & 0x00ff_ffff;
                        }
                    }
                }
                let request = {
                    let shared = state.lock().unwrap();
                    update_request(shared.frame.width, shared.frame.height, true)
                };
                // Cap empty-update loops and keep control input responsive.
                tokio::time::sleep(Duration::from_millis(16)).await;
                requests.send(request).await?;
            }
            2 => {} // Bell
            3 => {
                let mut padding = [0; 3];
                read.read_exact(&mut padding).await?;
                let _clipboard = bounded_text(&mut read).await?;
            }
            message => return Err(format!("unsupported RFB server message {message}").into()),
        }
    }
}

async fn protocol(
    url: String,
    state: State,
    mut input: mpsc::Receiver<Vec<u8>>,
    requests: mpsc::Sender<Vec<u8>>,
) -> Result<()> {
    let (mut stream, transport) = tokio::io::duplex(256 * 1024);
    let bridge = bridge(&url, transport);
    let viewer = async {
        let frame = tokio::time::timeout(Duration::from_secs(15), handshake(&mut stream)).await??;
        {
            let mut shared = state.lock().unwrap();
            shared.session.state = VncConnectionState::Connected {
                width: frame.width,
                height: frame.height,
            };
            shared.frame = frame;
        }
        let (read, mut write) = tokio::io::split(stream);
        let updates = read_updates(read, state, requests);
        let events = async {
            while let Some(event) = input.recv().await {
                write.write_all(&event).await?;
            }
            Result::Ok(())
        };
        tokio::select! { result = updates => result, result = events => result }
    };
    tokio::select! { result = bridge => result, result = viewer => result }
}

fn key_event(keysym: u32, down: bool) -> Vec<u8> {
    let mut message = vec![4, u8::from(down), 0, 0];
    message.extend(keysym.to_be_bytes());
    message
}
fn pointer_event(mask: u8, x: u16, y: u16) -> Vec<u8> {
    let mut message = vec![5, mask];
    message.extend(x.to_be_bytes());
    message.extend(y.to_be_bytes());
    message
}

// US keyboard mapping. IME/preedit and clipboard synchronization are not supported.
fn keysym(key: Key, shift: bool) -> Option<u32> {
    use Key::*;
    let letters = [
        A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    ];
    if let Some(i) = letters.iter().position(|&k| k == key) {
        return Some((if shift { b'A' } else { b'a' }) as u32 + i as u32);
    }
    let numbers = [Key0, Key1, Key2, Key3, Key4, Key5, Key6, Key7, Key8, Key9];
    if let Some(i) = numbers.iter().position(|&k| k == key) {
        return Some(
            (if shift {
                b")!@#$%^&*("[i]
            } else {
                b'0' + i as u8
            }) as u32,
        );
    }
    Some(match key {
        Space => 0x20,
        Enter | NumPadEnter => 0xff0d,
        Tab => 0xff09,
        Backspace => 0xff08,
        Escape => 0xff1b,
        Delete => 0xffff,
        Insert => 0xff63,
        Home => 0xff50,
        End => 0xff57,
        PageUp => 0xff55,
        PageDown => 0xff56,
        Left => 0xff51,
        Up => 0xff52,
        Right => 0xff53,
        Down => 0xff54,
        LeftShift => 0xffe1,
        RightShift => 0xffe2,
        LeftCtrl => 0xffe3,
        RightCtrl => 0xffe4,
        LeftAlt => 0xffe9,
        RightAlt => 0xffea,
        LeftSuper => 0xffeb,
        RightSuper => 0xffec,
        CapsLock => 0xffe5,
        NumLock => 0xff7f,
        ScrollLock => 0xff14,
        F1 => 0xffbe,
        F2 => 0xffbf,
        F3 => 0xffc0,
        F4 => 0xffc1,
        F5 => 0xffc2,
        F6 => 0xffc3,
        F7 => 0xffc4,
        F8 => 0xffc5,
        F9 => 0xffc6,
        F10 => 0xffc7,
        F11 => 0xffc8,
        F12 => 0xffc9,
        Minus => (if shift { b'_' } else { b'-' }) as u32,
        Equal => (if shift { b'+' } else { b'=' }) as u32,
        LeftBracket => (if shift { b'{' } else { b'[' }) as u32,
        RightBracket => (if shift { b'}' } else { b']' }) as u32,
        Backslash => (if shift { b'|' } else { b'\\' }) as u32,
        Semicolon => (if shift { b':' } else { b';' }) as u32,
        Apostrophe => (if shift { b'"' } else { b'\'' }) as u32,
        Comma => (if shift { b'<' } else { b',' }) as u32,
        Period => (if shift { b'>' } else { b'.' }) as u32,
        Slash => (if shift { b'?' } else { b'/' }) as u32,
        Backquote => (if shift { b'~' } else { b'`' }) as u32,
        NumPad0 => 0xffb0,
        NumPad1 => 0xffb1,
        NumPad2 => 0xffb2,
        NumPad3 => 0xffb3,
        NumPad4 => 0xffb4,
        NumPad5 => 0xffb5,
        NumPad6 => 0xffb6,
        NumPad7 => 0xffb7,
        NumPad8 => 0xffb8,
        NumPad9 => 0xffb9,
        NumPadDot => 0xffae,
        NumPadSlash => 0xffaf,
        NumPadAsterisk => 0xffaa,
        NumPadMinus => 0xffad,
        NumPadPlus => 0xffab,
        _ => return None,
    })
}

pub fn run(endpoint: &str) -> std::result::Result<(), String> {
    let mut session = RemoteSession::new(endpoint)?;
    session.state = VncConnectionState::Connecting;
    let state = Arc::new(Mutex::new(Shared {
        session,
        frame: Frame::default(),
    }));
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let (send, receive) = mpsc::channel(256);
    let worker_state = state.clone();
    let worker_send = send.clone();
    let endpoint = endpoint.to_owned();
    let task = runtime.spawn(async move {
        let result = protocol(endpoint, worker_state.clone(), receive, worker_send).await;
        worker_state.lock().unwrap().session.state = match result {
            Ok(()) => VncConnectionState::Disconnected,
            Err(error) => VncConnectionState::Failed(error.to_string()),
        };
    });
    let result = display(state, send);
    task.abort();
    // Dropping the runtime closes the WebSocket and all transport tasks.
    result.map_err(|e| e.to_string())
}

fn display(state: State, send: mpsc::Sender<Vec<u8>>) -> Result<()> {
    let mut window = Window::new("Velink — Connecting", 640, 360, WindowOptions::default())?;
    window.set_target_fps(60);
    let mut dimensions = (0, 0);
    let mut pressed = HashMap::new();
    let mut pointer = (0u8, 0u16, 0u16);
    let transmit = |message| {
        send.try_send(message)
            .map_err(|e| format!("input queue: {e}"))
    };
    while window.is_open() {
        {
            let shared = state.lock().unwrap();
            match &shared.session.state {
                VncConnectionState::Failed(error) => return Err(error.clone().into()),
                VncConnectionState::Disconnected => {
                    return Err("remote desktop disconnected".into());
                }
                VncConnectionState::Connected { width, height } => {
                    if dimensions != (*width, *height) {
                        window = Window::new(
                            "Velink — Remote desktop",
                            *width as usize,
                            *height as usize,
                            WindowOptions::default(),
                        )?;
                        window.set_target_fps(60);
                        dimensions = (*width, *height);
                    }
                    window.update_with_buffer(
                        &shared.frame.pixels,
                        *width as usize,
                        *height as usize,
                    )?;
                }
                _ => window.update(),
            }
        }
        if dimensions == (0, 0) {
            continue;
        }
        let keys = if window.is_active() {
            window.get_keys()
        } else {
            Vec::new()
        };
        let released: Vec<_> = pressed
            .keys()
            .copied()
            .filter(|key| !keys.contains(key))
            .collect();
        for key in released {
            transmit(key_event(pressed.remove(&key).unwrap(), false))?;
        }
        let shift = keys.contains(&Key::LeftShift) || keys.contains(&Key::RightShift);
        // Send modifier presses before ordinary keys, regardless of backend key order.
        let mut keys = keys;
        keys.sort_by_key(|key| {
            !matches!(
                key,
                Key::LeftShift
                    | Key::RightShift
                    | Key::LeftCtrl
                    | Key::RightCtrl
                    | Key::LeftAlt
                    | Key::RightAlt
                    | Key::LeftSuper
                    | Key::RightSuper
            )
        });
        for key in keys {
            if let std::collections::hash_map::Entry::Vacant(entry) = pressed.entry(key) {
                if let Some(symbol) = keysym(key, shift) {
                    transmit(key_event(symbol, true))?;
                    entry.insert(symbol);
                }
            }
        }
        if !window.is_active() {
            if pointer.0 != 0 {
                pointer.0 = 0;
                transmit(pointer_event(0, pointer.1, pointer.2))?;
            }
            continue;
        }
        if let Some((x, y)) = window.get_mouse_pos(MouseMode::Clamp) {
            let x = (x as u16).min(dimensions.0 - 1);
            let y = (y as u16).min(dimensions.1 - 1);
            let mask = u8::from(window.get_mouse_down(MouseButton::Left))
                | (u8::from(window.get_mouse_down(MouseButton::Middle)) << 1)
                | (u8::from(window.get_mouse_down(MouseButton::Right)) << 2);
            if (mask, x, y) != pointer {
                transmit(pointer_event(mask, x, y))?;
                pointer = (mask, x, y);
            }
            if let Some((horizontal, vertical)) = window.get_scroll_wheel() {
                for (amount, positive, negative) in [(vertical, 8, 16), (horizontal, 32, 64)] {
                    let button = if amount > 0.0 { positive } else { negative };
                    for _ in 0..(amount.abs().ceil() as usize).min(10) {
                        transmit(pointer_event(mask | button, x, y))?;
                        transmit(pointer_event(mask, x, y))?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn negotiates_and_decodes_fragmented_frame() {
        let (mut client, mut server) = tokio::io::duplex(64);
        let fake = tokio::spawn(async move {
            server.write_all(b"RFB 003.008\n").await.unwrap();
            let mut version = [0; 12];
            server.read_exact(&mut version).await.unwrap();
            assert_eq!(&version, b"RFB 003.008\n");
            server.write_all(&[1, 1]).await.unwrap();
            assert_eq!(server.read_u8().await.unwrap(), 1);
            server.write_all(&[0; 4]).await.unwrap();
            assert_eq!(server.read_u8().await.unwrap(), 1);
            server.write_all(&[0, 2, 0, 1]).await.unwrap();
            server.write_all(&[0; 16]).await.unwrap();
            server.write_all(&[0; 4]).await.unwrap();
            let mut setup = [0; 38];
            server.read_exact(&mut setup).await.unwrap();
            assert_eq!(
                &setup[..20],
                &[
                    0, 0, 0, 0, 32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0
                ]
            );
            assert_eq!(&setup[20..28], &[2, 0, 0, 1, 0, 0, 0, 0]);
            assert_eq!(&setup[28..], &update_request(2, 1, false));
            // Two pixels split into separate writes, independent of WebSocket boundaries.
            for byte in [
                0, 0, 0, 1, 0, 0, 0, 0, 0, 2, 0, 1, 0, 0, 0, 0, 0x33, 0x22, 0x11, 0, 0x66, 0x55,
                0x44, 0,
            ] {
                server.write_all(&[byte]).await.unwrap();
            }
        });
        let frame = handshake(&mut client).await.unwrap();
        let state = Arc::new(Mutex::new(Shared {
            session: RemoteSession::new("ws://localhost/vnc").unwrap(),
            frame,
        }));
        let (send, _receive) = mpsc::channel(8);
        assert!(read_updates(client, state.clone(), send).await.is_err()); // peer closes
        assert_eq!(state.lock().unwrap().frame.pixels, [0x112233, 0x445566]);
        fake.await.unwrap();
    }

    #[test]
    fn input_wire_format() {
        assert_eq!(key_event(0xffe3, true), [4, 1, 0, 0, 0, 0, 0xff, 0xe3]);
        assert_eq!(pointer_event(5, 300, 400), [5, 5, 1, 44, 1, 144]);
        assert_eq!(keysym(Key::A, true), Some(65));
    }
}
