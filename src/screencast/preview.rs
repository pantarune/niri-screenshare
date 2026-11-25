//! One-frame PipeWire captures for the picker. The portal owns each temporary
//! Mutter session so it can stop it even if the picker is killed.
use std::io::{BufRead, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use gstreamer as gst;
use gstreamer_video::{prelude::*, VideoCropMeta, VideoFrameRef, VideoInfo};

pub(super) const DEADLINE: Duration = Duration::from_secs(3);
pub(super) const WORKERS: usize = 3;
const MAX_EDGE: i32 = 480;

pub(super) async fn open_session(
    conn: &zbus::Connection,
    target: &super::pick::PickerChoice,
) -> anyhow::Result<(String, u32)> {
    use super::pick::PickerChoice;
    use tokio::time::timeout;

    let session = timeout(DEADLINE, super::create_niri_session(conn)).await??;
    let start = async {
        let stream = match target {
            PickerChoice::Monitor(name) => {
                super::record_niri_monitor(conn, &session, name, 0).await?
            }
            PickerChoice::Window(id) => super::record_niri_window(conn, &session, *id, 0).await?,
        };
        super::start_and_get_node_id(conn, &session, &stream, false, false).await
    };
    match timeout(DEADLINE, start)
        .await
        .context("preview node timeout")
        .and_then(|r| r)
    {
        Ok(node) => Ok((session, node)),
        Err(e) => {
            let _ = timeout(DEADLINE, super::stop_niri_session(conn, &session)).await;
            Err(e)
        }
    }
}

pub(super) struct Thumbnail {
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
}

/// The parent owns the session. The child receives only its PipeWire node.
pub(super) fn capture(index: usize, closed: &AtomicBool) -> anyhow::Result<Thumbnail> {
    gst::init()?;
    let mut reply = String::new();
    {
        // Keep the request and reply together when several cards load at once.
        let mut input = std::io::stdin().lock();
        anyhow::ensure!(!closed.load(Ordering::Relaxed), "picker closed");
        writeln!(std::io::stdout().lock(), "OPEN {index}")?;
        input.read_line(&mut reply)?;
    }
    let node = reply
        .trim_end()
        .parse()
        .context("preview node unavailable")?;
    let frame = snapshot(node, closed);
    writeln!(std::io::stdout().lock(), "STOP {index}")?;
    frame
}

fn snapshot(node: u32, closed: &AtomicBool) -> anyhow::Result<Thumbnail> {
    // Fix square pixels on the source, then let the scaler preserve the actual
    // frame aspect ratio, even if the window has resized since enumeration.
    let caps = format!(
        "format=RGBA,width=(int)[1,{MAX_EDGE}],height=(int)[1,{MAX_EDGE}],pixel-aspect-ratio=1/1"
    );
    let gpu = format!("glupload name=upload ! glcolorconvert ! video/x-raw(memory:GLMemory),format=RGBA ! glcolorscale ! video/x-raw(memory:GLMemory),{caps} ! gldownload");
    let end = Instant::now() + DEADLINE;
    let mut error = anyhow!("capture cancelled");
    for converter in [gpu.as_str(), "videoconvertscale"] {
        if closed.load(Ordering::Relaxed) || Instant::now() >= end {
            break;
        }
        let pipeline = format!("pipewiresrc path={node} min-buffers=2 max-buffers=3 ! video/x-raw(ANY),pixel-aspect-ratio=1/1 ! {converter} ! video/x-raw,{caps} ! appsink name=frame max-buffers=1 drop=true sync=false async=false");
        match capture_pipeline(&pipeline, closed, end) {
            Ok(frame) => return Ok(frame),
            Err(e) => error = e,
        }
    }
    Err(error)
}

fn capture_pipeline(
    description: &str,
    closed: &AtomicBool,
    end: Instant,
) -> anyhow::Result<Thumbnail> {
    let pipeline = gst::parse::launch(description)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("expected a GStreamer pipeline"))?;
    let sink = pipeline
        .by_name("frame")
        .and_then(|s| s.downcast::<gstreamer_app::AppSink>().ok())
        .context("missing preview appsink")?;
    // GL scales the whole texture. Let the software converter apply a producer's
    // crop if a frame only contains a valid subregion.
    let cropped = Arc::new(AtomicBool::new(false));
    if let Some(upload) = pipeline.by_name("upload") {
        let cropped = cropped.clone();
        let pad = upload.static_pad("sink").context("missing GL upload pad")?;
        pad.add_probe(gst::PadProbeType::BUFFER, move |pad, probe| {
            if let Some(meta) = probe.buffer().and_then(|b| b.meta::<VideoCropMeta>()) {
                let caps = pad.current_caps();
                let size = caps.as_ref().and_then(|c| c.structure(0)).and_then(|s| {
                    Some((
                        s.get::<i32>("width").ok()? as u32,
                        s.get::<i32>("height").ok()? as u32,
                    ))
                });
                let (x, y, w, h) = meta.rect();
                if x != 0 || y != 0 || size != Some((w, h)) {
                    cropped.store(true, Ordering::Relaxed);
                    return gst::PadProbeReturn::Drop;
                }
            }
            gst::PadProbeReturn::Ok
        });
    }
    let result = (|| {
        pipeline.set_state(gst::State::Playing)?;
        let bus = pipeline.bus().context("missing pipeline bus")?;
        while !closed.load(Ordering::Relaxed) && Instant::now() < end {
            if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(50)) {
                let info = VideoInfo::from_caps(sample.caps().context("missing video caps")?)?;
                let buf = sample.buffer().context("missing video buffer")?;
                if buf
                    .flags()
                    .intersects(gst::BufferFlags::CORRUPTED | gst::BufferFlags::GAP)
                {
                    continue;
                }
                let frame = VideoFrameRef::from_buffer_ref_readable(buf, &info)?;
                let (x, y, w, h) = buf.meta::<VideoCropMeta>().map(|m| m.rect()).unwrap_or((
                    0,
                    0,
                    info.width(),
                    info.height(),
                ));
                anyhow::ensure!(
                    w > 0
                        && h > 0
                        && info.width() <= MAX_EDGE as u32
                        && info.height() <= MAX_EDGE as u32
                        && x.checked_add(w).is_some_and(|v| v <= info.width())
                        && y.checked_add(h).is_some_and(|v| v <= info.height()),
                    "invalid preview region"
                );
                let stride: usize = frame.plane_stride()[0].try_into()?;
                let data = frame.plane_data(0)?;
                let mut rgba = Vec::with_capacity((w * h * 4) as usize);
                for row in y..y + h {
                    let start = row as usize * stride + x as usize * 4;
                    rgba.extend_from_slice(
                        data.get(start..start + w as usize * 4)
                            .context("short preview row")?,
                    );
                }
                return Ok(Thumbnail {
                    width: w as i32,
                    height: h as i32,
                    rgba,
                });
            }
            if let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error]) {
                if let gst::MessageView::Error(e) = msg.view() {
                    anyhow::bail!("{}", e.error());
                }
            }
            if sink.is_eos() || cropped.load(Ordering::Relaxed) {
                break;
            }
        }
        anyhow::bail!("preview ended without a frame")
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_pipeline_returns_owned_rgba_rows() {
        gst::init().unwrap();
        let frame = capture_pipeline("videotestsrc num-buffers=1 pattern=red ! video/x-raw,width=80,height=60 ! videoconvertscale ! video/x-raw,format=RGBA,width=40,height=30 ! appsink name=frame max-buffers=1 drop=true sync=false async=false",
            &AtomicBool::new(false), Instant::now()+DEADLINE).unwrap();
        assert_eq!((frame.width, frame.height), (40, 30));
        assert_eq!(frame.rgba.len(), 40 * 30 * 4);
        assert!(frame
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[0] > 240 && p[1] < 8 && p[2] < 8 && p[3] == 255));
    }

    #[tokio::test]
    #[ignore = "requires a running niri desktop and PipeWire"]
    async fn live_capture_all_targets() {
        use super::super::pick::PickerChoice;
        use crate::niri_ipc::{list_outputs, list_windows};
        gst::init().unwrap();
        let conn = zbus::Connection::session().await.unwrap();
        let mut targets: Vec<_> = list_outputs()
            .unwrap()
            .into_iter()
            .map(|o| {
                (
                    PickerChoice::Monitor(o.name),
                    (o.logical.width, o.logical.height),
                )
            })
            .collect();
        assert!(!targets.is_empty());
        targets.extend(
            list_windows()
                .unwrap()
                .into_iter()
                .map(|w| (PickerChoice::Window(w.id), (w.size.width, w.size.height))),
        );
        for (target, size) in targets {
            let (session, node) = open_session(&conn, &target).await.unwrap();
            let frame = snapshot(node, &AtomicBool::new(false));
            super::super::stop_niri_session(&conn, &session).await;
            let frame = frame.unwrap_or_else(|e| panic!("{target:?}: {e:#}"));
            assert!(frame.width > 0 && frame.height > 0);
            if size.0.max(size.1) >= MAX_EDGE {
                assert_eq!(frame.width.max(frame.height), MAX_EDGE);
            }
        }
    }
}
