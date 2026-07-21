use std::os::fd::BorrowedFd;

use anyhow::Context;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_shm::{self, Format as ShmFormat, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{
    self, ZwlrScreencopyFrameV1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

#[derive(Default, Clone)]
pub struct FrameData {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub ready: bool,
}

#[derive(Default)]
pub struct CaptureState {
    manager: Option<ZwlrScreencopyManagerV1>,
    shm: Option<WlShm>,
    pub outputs: Vec<(WlOutput, i32, i32)>,
    frame: Option<ZwlrScreencopyFrameV1>,
    frame_data: FrameData,
    frame_done: bool,
    shm_format: Option<(ShmFormat, u32, u32)>, // format, width, height from buffer event
}

pub struct ScreencopyCapture {
    conn: Connection,
    event_queue: EventQueue<CaptureState>,
    qh: QueueHandle<CaptureState>,
    state: std::rc::Rc<std::cell::RefCell<CaptureState>>,
    pool_fd: Option<std::os::raw::c_int>,
    pool: Option<WlShmPool>,
    buffer: Option<WlBuffer>,
    buf_size: usize,
}

impl ScreencopyCapture {
    pub fn new() -> anyhow::Result<Self> {
        let conn = Connection::connect_to_env().context("failed to connect to wayland")?;
        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();
        let state = std::rc::Rc::new(std::cell::RefCell::new(CaptureState::default()));

        let _reg = conn.display().get_registry(&qh, ());
        event_queue
            .roundtrip(&mut *state.borrow_mut())
            .context("registry roundtrip")?;

        {
            let inner = state.borrow();
            if inner.manager.is_none() {
                anyhow::bail!("zwlr_screencopy_manager_v1 not available");
            }
        }

        Ok(Self {
            conn,
            event_queue,
            qh,
            state,
            pool_fd: None,
            pool: None,
            buffer: None,
            buf_size: 0,
        })
    }

    pub fn first_output(&self) -> Option<WlOutput> {
        self.state
            .borrow()
            .outputs
            .first()
            .map(|(o, _, _)| o.clone())
    }

    pub fn capture_frame(&mut self, output: &WlOutput) -> anyhow::Result<FrameData> {
        let manager = self
            .state
            .borrow()
            .manager
            .clone()
            .ok_or_else(|| anyhow::anyhow!("screencopy manager not available"))?;

        {
            let mut inner = self.state.borrow_mut();
            inner.frame_data = FrameData::default();
            inner.frame_done = false;
            inner.shm_format = None;
        }

        let _frame = manager.capture_output(1, output, &self.qh, ());
        {
            let mut inner = self.state.borrow_mut();
            inner.frame = Some(_frame);
        }

        self.event_queue
            .roundtrip(&mut *self.state.borrow_mut())
            .context("waiting for buffer info")?;

        let (fmt, w, h) = {
            let inner = self.state.borrow();
            inner.shm_format.unwrap_or((ShmFormat::Argb8888, 1920, 1080))
        };

        let w_actual = w.max(1);
        let h_actual = h.max(1);
        let stride = w_actual as i32 * 4;
        let size = stride as u64 * h_actual as u64;

        let fd = create_shm_file(size)?;
        let bfd = unsafe { BorrowedFd::borrow_raw(fd) };

        let pool = {
            let inner = self.state.borrow();
            let shm = inner
                .shm
                .clone()
                .ok_or_else(|| anyhow::anyhow!("wl_shm not available"))?;
            shm.create_pool(bfd, size as i32, &self.qh, ())
        };

        let buffer = pool.create_buffer(
            0, w_actual as i32, h_actual as i32, stride, fmt, &self.qh, (),
        );

        {
            let inner = self.state.borrow();
            if let Some(ref f) = inner.frame {
                f.copy(&buffer);
            }
        }
        let _ = self.event_queue.flush();

        self.state.borrow_mut().frame_done = false;
        for _ in 0..1000 {
            let _ = self
                .event_queue
                .dispatch_pending(&mut *self.state.borrow_mut());
            if self.state.borrow().frame_done {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        {
            let mut inner = self.state.borrow_mut();
            inner.frame.take();
        }

        let mut data = vec![0u8; size as usize];
        unsafe {
            libc::pread(fd, data.as_mut_ptr() as *mut libc::c_void, size as usize, 0);
        }

        drop(buffer);
        drop(pool);
        unsafe { libc::close(fd) };

        let nonzero = data.iter().filter(|&&b| b != 0).count();
        tracing::info!(
            "captured: {}x{} stride={} nonzero_bytes={}/{}",
            w_actual, h_actual, stride, nonzero, data.len()
        );

        Ok(FrameData {
            width: w_actual,
            height: h_actual,
            data,
            ready: nonzero > 0,
        })
    }
}

fn create_shm_file(size: u64) -> anyhow::Result<std::os::raw::c_int> {
    let fd = unsafe {
        libc::memfd_create(
            "screencopy\0".as_ptr() as *const libc::c_char,
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd >= 0 {
        if unsafe { libc::ftruncate(fd, size as libc::off_t) } < 0 {
            unsafe { libc::close(fd) };
            anyhow::bail!("ftruncate failed");
        }
        return Ok(fd);
    }
    let path = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let c_str = std::ffi::CString::new(format!("{}/screencopy-shm-XXXXXX", path))?;
    let mut buf = c_str.into_bytes_with_nul();
    let fd = unsafe { libc::mkstemp(buf.as_mut_ptr() as *mut libc::c_char) };
    if fd < 0 {
        anyhow::bail!("failed to create shm temp file");
    }
    unsafe { libc::unlink(buf.as_ptr() as *const libc::c_char); }
    if unsafe { libc::ftruncate(fd, size as libc::off_t) } < 0 {
        unsafe { libc::close(fd) };
        anyhow::bail!("ftruncate failed");
    }
    Ok(fd)
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for CaptureState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrScreencopyManagerV1,
        _event: <ZwlrScreencopyManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        proxy: &ZwlrScreencopyFrameV1,
        event: <ZwlrScreencopyFrameV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                state.frame_data.width = width;
                state.frame_data.height = height;
                let fmt = match format {
                    wayland_client::WEnum::Value(f) => f,
                    _ => wl_shm::Format::Argb8888,
                };
                state.shm_format = Some((fmt, width, height));
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => {}
            zwlr_screencopy_frame_v1::Event::Ready { .. } => {
                state.frame_done = true;
                state.frame_data.ready = true;
                proxy.destroy();
            }
            zwlr_screencopy_frame_v1::Event::Failed => {
                state.frame_done = true;
                state.frame_data.ready = false;
                proxy.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlShm, ()> for CaptureState {
    fn event(
        _state: &mut Self,
        _proxy: &WlShm,
        _event: <WlShm as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for CaptureState {
    fn event(
        _state: &mut Self,
        _proxy: &WlShmPool,
        _event: <WlShmPool as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for CaptureState {
    fn event(
        _state: &mut Self,
        _proxy: &WlBuffer,
        _event: <WlBuffer as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlOutput, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _proxy: &WlOutput,
        event: <WlOutput as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_output::Event;
        match event {
            Event::Geometry { x, y, .. } => {
                let idx = state.outputs.len().saturating_sub(1);
                if let Some((_, ref mut x_, ref mut y_)) = state.outputs.get_mut(idx) {
                    *x_ = x;
                    *y_ = y;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, ()> for CaptureState {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: <WlRegistry as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_registry::Event;
        match event {
            Event::Global {
                name,
                interface,
                version,
            } => {
                if interface == "zwlr_screencopy_manager_v1" {
                    let manager = registry.bind::<ZwlrScreencopyManagerV1, _, _>(
                        name, version.min(3), qh, (),
                    );
                    state.manager = Some(manager);
                } else if interface == "wl_shm" {
                    let shm = registry.bind::<WlShm, _, _>(name, 1, qh, ());
                    state.shm = Some(shm);
                } else if interface == "wl_output" {
                    let output = registry.bind::<WlOutput, _, _>(name, 1, qh, ());
                    state.outputs.push((output, 0, 0));
                }
            }
            Event::GlobalRemove { name: _ } => {}
            _ => {}
        }
    }
}

pub fn capture_frame_for_output(_output_name: &str) -> anyhow::Result<FrameData> {
    let mut screencopy = ScreencopyCapture::new()?;
    let output = screencopy
        .first_output()
        .ok_or_else(|| anyhow::anyhow!("no output"))?;
    let frame = screencopy.capture_frame(&output)?;
    Ok(frame)
}
