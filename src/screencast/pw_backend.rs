use std::os::fd::FromRawFd;
use std::os::unix::io::{IntoRawFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::Context;

pub fn open_pipewire_fd(_session_handle: &str) -> anyhow::Result<RawFd> {
    let socket_path = std::env::var("PIPEWIRE_REMOTE").unwrap_or_else(|_| {
        let runtime =
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".to_string());
        format!("{}/pipewire-0", runtime)
    });

    let socket = std::os::unix::net::UnixStream::connect(&socket_path)
        .with_context(|| format!("connecting to pipewire socket at {}", socket_path))?;

    Ok(socket.into_raw_fd())
}

pub struct StreamHandle {
    pub node_id: Arc<AtomicU32>,
    pub stop_flag: Arc<AtomicBool>,
}

pub fn create_screencast_stream(width: u32, height: u32) -> anyhow::Result<StreamHandle> {
    pipewire::init();

    let node_id = Arc::new(AtomicU32::new(0));
    let stop_flag = Arc::new(AtomicBool::new(false));
    let node_id_c = node_id.clone();
    let stop_c = stop_flag.clone();

    std::thread::spawn(move || {
        if let Err(e) = run_stream_thread(node_id_c, stop_c, width, height) {
            tracing::error!("stream thread: {e}");
        }
    });

    for _ in 0..200 {
        let id = node_id.load(Ordering::Relaxed);
        if id != 0 && id != u32::MAX {
            return Ok(StreamHandle { node_id, stop_flag });
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    anyhow::bail!("pipewire stream failed to connect within timeout");
}

fn run_stream_thread(
    node_id: Arc<AtomicU32>,
    stop_flag: Arc<AtomicBool>,
    width: u32,
    height: u32,
) -> anyhow::Result<()> {
    let mainloop = pipewire::main_loop::MainLoopBox::new(None)?;
    let context = pipewire::context::ContextBox::new(&mainloop.loop_(), None)?;
    let core = context.connect(None)?;

    let mut props = pipewire::properties::PropertiesBox::new();
    props.insert("media.type", "Video");
    props.insert("media.category", "Capture");
    props.insert("media.role", "Screen");
    props.insert("media.class", "Video/Source");
    props.insert("node.name", "niri-screencast");
    props.insert("media.name", "niri-screencast");
    props.insert("stream.is-live", "true");
    props.insert("node.autoconnect", "true");

    let stream = pipewire::stream::StreamBox::new(&core, "niri-screencast", props)?;

    let _capture_cell: std::rc::Rc<std::cell::RefCell<Option<crate::screencopy::ScreencopyCapture>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture_cell2 = _capture_cell.clone();

    let _listener = stream
        .add_local_listener::<()>()
        .state_changed(move |s, _ud, _old, new| {
            use pipewire::stream::StreamState;
            if new == StreamState::Paused || new == StreamState::Streaming {
                node_id.store(s.node_id(), Ordering::Relaxed);
            }
        })
        .process(move |_stream, _ud| {
            let mut cell = capture_cell2.borrow_mut();
            if cell.is_none() {
                *cell = crate::screencopy::ScreencopyCapture::new().ok();
            }

            if let Some(ref mut screencopy) = *cell {
                if let Some(ref out) = screencopy.first_output() {
                    if let Ok(frame) = screencopy.capture_frame(out) {
                        if let Some(mut buf) = _stream.dequeue_buffer() {
                            if frame.ready && frame.data.len() >= 4 {
                                let datas = buf.datas_mut();
                                if let Some(d) = datas.first_mut() {
                                    if let Some(slice) = d.data() {
                                        let copy_len = slice.len().min(frame.data.len());
                                        slice[..copy_len].copy_from_slice(&frame.data[..copy_len]);
                                        let chunk = d.chunk_mut();
                                        *chunk.size_mut() = copy_len as u32;
                                        *chunk.offset_mut() = 0;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
        .register()
        .map_err(|e| anyhow::anyhow!("pipewire listener: {e}"))?;

    let obj = pipewire::spa::pod::object!(
        pipewire::spa::utils::SpaTypes::ObjectParamFormat,
        pipewire::spa::param::ParamType::EnumFormat,
        pipewire::spa::pod::property!(
            pipewire::spa::param::format::FormatProperties::MediaType,
            Id,
            pipewire::spa::param::format::MediaType::Video
        ),
        pipewire::spa::pod::property!(
            pipewire::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pipewire::spa::param::format::MediaSubtype::Raw
        ),
        pipewire::spa::pod::property!(
            pipewire::spa::param::format::FormatProperties::VideoFormat,
            Id,
            pipewire::spa::param::video::VideoFormat::BGRx
        ),
        pipewire::spa::pod::property!(
            pipewire::spa::param::format::FormatProperties::VideoSize,
            Rectangle,
            pipewire::spa::utils::Rectangle { width, height }
        ),
        pipewire::spa::pod::property!(
            pipewire::spa::param::format::FormatProperties::VideoFramerate,
            Fraction,
            pipewire::spa::utils::Fraction { num: 30, denom: 1 }
        ),
    );

    let values: Vec<u8> = pipewire::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pipewire::spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow::anyhow!("pod serialize: {e}"))?
    .0
    .into_inner();

    let mut params = [pipewire::spa::pod::Pod::from_bytes(&values)
        .ok_or_else(|| anyhow::anyhow!("invalid pod"))?];

    stream.connect(
        pipewire::spa::utils::Direction::Output,
        None,
        pipewire::stream::StreamFlags::AUTOCONNECT
            | pipewire::stream::StreamFlags::DONT_RECONNECT,
        &mut params,
    )?;

    mainloop.run();
    Ok(())
}
