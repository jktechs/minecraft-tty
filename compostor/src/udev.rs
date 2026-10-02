use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use calloop::{EventLoop, ping::PingSource};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent, DrmNode, NodeType,
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
        },
        egl::{EGLContext, EGLDisplay},
        input::{
            AbsolutePositionEvent, Axis, AxisSource, Event, InputBackend, InputEvent, KeyState,
            KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
        },
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            ImportAll, ImportDma, ImportMem,
            element::{
                AsRenderElements, Id, Kind, RenderElementPresentationState,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
        },
        session::{
            Event as SessionEvent, Session,
            libseat::{LibSeatSession, LibSeatSessionNotifier},
        },
        udev::primary_gpu,
    },
    desktop::utils::send_frames_surface_tree,
    input::{
        keyboard::{FilterResult, Keysym},
        pointer::{
            AxisFrame, ButtonEvent, CursorIcon, CursorImageStatus, CursorImageSurfaceData,
            MotionEvent, RelativeMotionEvent,
        },
    },
    output::OutputModeSource,
    reexports::{
        drm::control::{Device as ControlDevice, ModeTypeFlags, connector},
        input::{ClickMethod, Libinput},
        rustix::fs::OFlags,
        wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags,
    },
    render_elements,
    utils::{DeviceFd, Logical, Physical, Point, SERIAL_COUNTER, Scale, Transform},
    wayland::{
        compositor::with_states,
        dmabuf::{DmabufFeedback, DmabufFeedbackBuilder},
        seat::WaylandFocus,
    },
};

use crate::{LoopData, state::App};

type KmsCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

render_elements! {
    pub KmsElement<R> where R: ImportAll + ImportMem;
    Surface = WaylandSurfaceRenderElement<R>,
    Cursor = MemoryRenderBufferRenderElement<R>,
}

struct Frame {
    buffer: MemoryRenderBuffer,
    hotspot: Point<i32, Logical>,
    delay_ms: u32,
}

struct CursorSprite {
    frames: Vec<Frame>,
    total_ms: u32,
}

impl CursorSprite {
    fn frame(&self, elapsed: Duration) -> &Frame {
        if self.frames.len() == 1 || self.total_ms == 0 {
            return &self.frames[0];
        }
        let mut t = (elapsed.as_millis() as u32) % self.total_ms;
        for f in &self.frames {
            if t < f.delay_ms {
                return f;
            }
            t -= f.delay_ms;
        }
        &self.frames[0]
    }

    fn fallback() -> Self {
        // 8x8 white square, hotspot at the corner
        let buffer = MemoryRenderBuffer::from_slice(
            &vec![255u8; 8 * 8 * 4],
            Fourcc::Abgr8888,
            (8, 8),
            1,
            Transform::Normal,
            None,
        );
        Self {
            frames: vec![Frame {
                buffer,
                hotspot: (0, 0).into(),
                delay_ms: 0,
            }],
            total_ms: 0,
        }
    }
}

struct CursorCache {
    theme: xcursor::CursorTheme,
    size: i32,
    map: HashMap<CursorIcon, CursorSprite>,
}

impl CursorCache {
    fn new() -> Self {
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(24);
        let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
        Self {
            theme: xcursor::CursorTheme::load(&theme),
            size,
            map: HashMap::new(),
        }
    }

    fn get(&mut self, icon: CursorIcon) -> &CursorSprite {
        let theme = &self.theme;
        let size = self.size;
        self.map
            .entry(icon)
            .or_insert_with(|| Self::load(theme, size, icon).unwrap_or_else(CursorSprite::fallback))
    }

    fn load(theme: &xcursor::CursorTheme, want: i32, icon: CursorIcon) -> Option<CursorSprite> {
        // spec name, legacy aliases, then generic fallbacks
        let mut names = std::iter::once(icon.name())
            .chain(icon.alt_names().iter().copied())
            .chain(["default", "left_ptr"]);

        let images = names.find_map(|n| {
            let path = theme.load_icon(n)?;
            let data = std::fs::read(path).ok()?;
            xcursor::parser::parse_xcursor(&data)
        })?;

        // nominal size closest to the wanted one; all images of that size are the animation frames
        let best = images
            .iter()
            .map(|i| i.size)
            .min_by_key(|&s| (s as i32 - want).abs())?;

        let frames: Vec<Frame> = images
            .into_iter()
            .filter(|i| i.size == best)
            .map(|i| Frame {
                // pixels_rgba is R,G,B,A in memory order, i.e. Abgr8888 as a little-endian u32
                buffer: MemoryRenderBuffer::from_slice(
                    &i.pixels_rgba,
                    Fourcc::Abgr8888,
                    (i.width as i32, i.height as i32),
                    1,
                    Transform::Normal,
                    None,
                ),
                hotspot: (i.xhot as i32, i.yhot as i32).into(),
                delay_ms: i.delay,
            })
            .collect();

        let total_ms = frames.iter().map(|f| f.delay_ms).sum();
        Some(CursorSprite { frames, total_ms })
    }
}

struct Pending {
    session: LibSeatSessionNotifier,
    input: LibinputInputBackend,
    drm: DrmDeviceNotifier,
    ping: PingSource,
}

pub struct UdevBackend {
    pub session: LibSeatSession,
    libinput: Libinput,
    drm: DrmDevice,
    renderer: GlesRenderer,
    compositor: KmsCompositor,
    mode: smithay::output::Mode,
    flip_pending: bool,
    start: Instant,
    pending: Option<Pending>,
    cursors: CursorCache,
    pub render_feedback: DmabufFeedback,
    scanout_feedback: DmabufFeedback,
}

impl std::fmt::Debug for UdevBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KmsBackend")
            .field("mode", &self.mode)
            .field("flip_pending", &self.flip_pending)
            .finish_non_exhaustive()
    }
}
impl UdevBackend {
    pub fn new(redraw: PingSource) -> Result<Self, Box<dyn std::error::Error>> {
        let (mut session, session_notifier) = LibSeatSession::new()?;
        let seat = session.seat();

        // libinput, devices opened through the session so VT switches revoke them
        let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(
            session.clone().into(),
        );
        libinput
            .udev_assign_seat(&seat)
            .map_err(|_| "libinput udev_assign_seat failed")?;
        let input = LibinputInputBackend::new(libinput.clone());

        // DRM node via the session
        let path = primary_gpu(&seat)?.ok_or("no GPU found for seat")?;
        let fd = session.open(
            &path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )?;
        let drm_fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (mut drm, drm_notifier) = DrmDevice::new(drm_fd.clone(), true)?;

        // GBM, EGL, GLES on the same device
        let gbm = GbmDevice::new(drm_fd.clone())?;
        let egl = unsafe { EGLDisplay::new(gbm.clone()) }?;
        let context = EGLContext::new(&egl)?;
        let renderer = unsafe { GlesRenderer::new(context)? };

        // First connected connector, preferred mode, first compatible CRTC
        let res = drm.resource_handles()?;
        let (conn, drm_mode, crtc) = res
            .connectors()
            .iter()
            .find_map(|&h| {
                let info = drm.get_connector(h, false).ok()?;
                if info.state() != connector::State::Connected {
                    return None;
                }
                let mode = info
                    .modes()
                    .iter()
                    .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
                    .or(info.modes().first())
                    .copied()?;
                let crtc = info
                    .encoders()
                    .iter()
                    .filter_map(|&e| drm.get_encoder(e).ok())
                    .flat_map(|e| res.filter_crtcs(e.possible_crtcs()))
                    .next()?;
                Some((h, mode, crtc))
            })
            .ok_or("no connected connector")?;

        let (w, h) = drm_mode.size();
        let mode = smithay::output::Mode {
            size: (w as i32, h as i32).into(),
            refresh: drm_mode.vrefresh() as i32 * 1000,
        };

        let surface = drm.create_surface(crtc, drm_mode, &[conn])?;
        let planes = surface.planes().clone();

        let allocator = GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );

        let node = DrmNode::from_file(&drm_fd)?;
        let dev = node
            .node_with_type(NodeType::Render)
            .and_then(Result::ok)
            .unwrap_or(node)
            .dev_id();

        let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node));
        let render_formats = renderer
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect::<Vec<_>>();

        let compositor = DrmCompositor::new(
            OutputModeSource::Static {
                size: mode.size,
                scale: Scale::from(1.),
                transform: Transform::Normal,
            },
            surface,
            Some(planes),
            allocator,
            exporter,
            [Fourcc::Xrgb8888, Fourcc::Argb8888],
            render_formats.clone(),
            drm.cursor_size(),
            Some(gbm),
        )?;

        let render_feedback = DmabufFeedbackBuilder::new(dev, render_formats.clone()).build()?;
        let scanout_feedback = DmabufFeedbackBuilder::new(dev, render_formats)
            .add_preference_tranche(
                dev,
                Some(TrancheFlags::Scanout),
                compositor.surface().plane_info().formats.iter().copied(),
            )
            .build()?;

        Ok(Self {
            session,
            libinput,
            drm,
            renderer,
            compositor,
            mode,
            flip_pending: false,
            start: Instant::now(),
            pending: Some(Pending {
                session: session_notifier,
                input,
                drm: drm_notifier,
                ping: redraw,
            }),
            cursors: CursorCache::new(),
            render_feedback,
            scanout_feedback,
        })
    }

    pub fn import_pending(&mut self, app: &mut App) {
        for (dmabuf, notifier) in app.pending_dmabufs.drain(..) {
            match self.renderer.import_dmabuf(&dmabuf, None) {
                Ok(_) => {
                    let _ = notifier.successful::<App>();
                }
                Err(_) => notifier.failed(),
            }
        }
    }

    pub fn register_loop(
        &mut self,
        event_loop: &EventLoop<LoopData>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Pending {
            session,
            input,
            drm,
            ping,
        } = self.pending.take().unwrap();

        let handle = event_loop.handle();
        handle.insert_source(input, |mut event, _, loop_data| {
            if let InputEvent::DeviceAdded { device } = &mut event
                && device.config_tap_finger_count() > 0
            {
                let _ = device.config_tap_set_enabled(true);
                // let _ = device.config_tap_set_button_map(TapButtonMap::LeftRightMiddle); // 1/2/3 finger tap
                let _ = device.config_tap_set_drag_enabled(true);
                let _ = device.config_click_set_method(ClickMethod::ButtonAreas); // physical click: 1/2/3 fingers
                let _ = device.config_dwt_set_enabled(false); // ignore touchpad while typing
                // let _ = d.config_scroll_set_natural_scroll_enabled(true);
            }
            loop_data.backend.input(&mut loop_data.app, event)
        })?;

        handle.insert_source(drm, |event, _, loop_data| match event {
            DrmEvent::VBlank(_) => {
                loop_data.backend.compositor.frame_submitted().unwrap();
                loop_data.backend.flip_pending = false;
                loop_data.backend.redraw(&mut loop_data.app).unwrap();
            }
            DrmEvent::Error(e) => eprintln!("drm error: {e}"),
        })?;

        handle.insert_source(session, |event, _, loop_data| match event {
            SessionEvent::PauseSession => {
                loop_data.backend.libinput.suspend();
                loop_data.backend.drm.pause();
            }
            SessionEvent::ActivateSession => {
                loop_data.backend.libinput.resume().unwrap();
                loop_data.backend.drm.activate(true).unwrap();
                loop_data.backend.compositor.reset_state().unwrap();
                loop_data.backend.flip_pending = false;
                loop_data.backend.redraw(&mut loop_data.app).unwrap();
            }
        })?;

        handle.insert_source(ping, |_, _, loop_data| {
            let _ = loop_data.backend.redraw(&mut loop_data.app);
        })?;

        // Output mode first, then the first frame, once App exists
        handle.insert_idle(|loop_data| {
            let mode = loop_data.backend.mode;
            loop_data
                .app
                .output
                .change_current_state(Some(mode), None, None, Some((0, 0).into()));
            loop_data.app.output.set_preferred(mode);
            // reconfigure any toplevel that already exists (usually none yet)
            loop_data.app.resize(mode.size);
            loop_data.backend.redraw(&mut loop_data.app).unwrap();
        });
        Ok(())
    }

    pub fn redraw(&mut self, state: &mut App) -> Result<(), Box<dyn std::error::Error>> {
        if !self.session.is_active() || self.flip_pending {
            return Ok(());
        }

        let elms = {
            let mut elms: Vec<KmsElement<GlesRenderer>> = Vec::new();

            if let Some(window) = state.surface.as_ref() {
                // Pointer location is stored surface-local (your abs path adds geometry.loc),
                // so subtract it to get output space.
                let loc =
                    state.seat_state.pointer.current_location() - window.geometry().loc.to_f64();

                let elapsed = self.start.elapsed();

                match &state.seat_state.image_status {
                    CursorImageStatus::Hidden => {}
                    CursorImageStatus::Named(icon) => {
                        let sprite = self.cursors.get(*icon);
                        let frame = sprite.frame(elapsed);
                        let pos = (loc - frame.hotspot.to_f64()).to_physical(1.);
                        if let Ok(cursor) = MemoryRenderBufferRenderElement::from_buffer(
                            &mut self.renderer,
                            pos,
                            &frame.buffer,
                            None,
                            None,
                            None,
                            Kind::Cursor,
                        ) {
                            elms.push(cursor.into());
                        }
                    }
                    CursorImageStatus::Surface(surface) => {
                        let hotspot = with_states(surface, |data| {
                            data.data_map
                                .get::<CursorImageSurfaceData>()
                                .map(|d| d.lock().unwrap().hotspot)
                                .unwrap_or_default()
                        });
                        let pos = (loc - hotspot.to_f64()).to_physical(1.).to_i32_round();
                        elms.extend(render_elements_from_surface_tree(
                            &mut self.renderer,
                            surface,
                            pos,
                            1.,
                            1.,
                            Kind::Cursor,
                        ));
                    }
                }

                elms.extend(
                    window
                        .render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
                            &mut self.renderer,
                            Point::<i32, Physical>::from((0, 0))
                                - window.geometry().loc.to_physical(1),
                            Scale { x: 1., y: 1. },
                            1.,
                        )
                        .into_iter()
                        .map(KmsElement::from),
                );
            }
            elms
        };

        let res = self.compositor.render_frame(
            &mut self.renderer,
            &elms,
            [0.5, 0.1, 0.1, 1.0],
            FrameFlags::DEFAULT,
        )?;
        let empty = res.is_empty;
        let states = res.states.clone();
        drop(res);

        if !empty {
            self.compositor.queue_frame(())?;
            self.flip_pending = true;
        }

        // Frame callbacks go out even for an empty frame, otherwise a client whose
        // commit carried no damage never gets its callback and stalls.
        let now: Duration = self.start.elapsed();
        if let CursorImageStatus::Surface(s) = &state.seat_state.image_status {
            send_frames_surface_tree(s, &state.output, now, None, |_, _| {
                Some(state.output.clone())
            });
        }
        if let Some(window) = state.surface.as_ref() {
            let out = state.output.clone();
            window.send_frame(&state.output, now, None, |_, _| Some(out.clone()));
            window.send_dmabuf_feedback(
                &state.output,
                |_, _| Some(out.clone()),
                |surface, _| {
                    let zero_copy = states
                        .states
                        .get(&Id::from_wayland_resource(surface))
                        .is_some_and(|s| {
                            s.presentation_state == RenderElementPresentationState::ZeroCopy
                        });
                    if zero_copy {
                        &self.scanout_feedback
                    } else {
                        &self.render_feedback
                    }
                },
            );
        }
        Ok(())
    }
    pub fn input<E: InputBackend>(&mut self, state: &mut App, event: InputEvent<E>) {
        enum Action {
            Quit,
            Vt(i32),
        }
        let surface = state.surface.as_ref().and_then(|x| x.wl_surface());
        let locked = state.seat_state.pointer_locked(surface.as_deref());
        match event {
            InputEvent::Keyboard { event } => {
                match state.seat_state.keyboard.clone().input(
                    state,
                    event.key_code(),
                    event.state(),
                    SERIAL_COUNTER.next_serial(),
                    event.time_msec(),
                    |_, mods, handle| {
                        if event.state() != KeyState::Pressed {
                            return FilterResult::Forward;
                        }
                        let sym = handle.modified_sym();
                        let raw = sym.raw();
                        // XF86Switch_VT_1..12 = 0x1008FE01..=0x1008FE0C; xkb yields these for Ctrl+Alt+Fn
                        if (0x1008FE01..=0x1008FE0C).contains(&raw) {
                            return FilterResult::Intercept(Action::Vt(
                                (raw - 0x1008FE01 + 1) as i32,
                            ));
                        }
                        if mods.ctrl && mods.alt && sym == Keysym::BackSpace {
                            return FilterResult::Intercept(Action::Quit);
                        }
                        FilterResult::Forward
                    },
                ) {
                    Some(Action::Quit) => state.exit(),
                    Some(Action::Vt(n)) => {
                        let _ = self.session.change_vt(n);
                    }
                    None => {}
                }
            }
            InputEvent::PointerAxis { event } => {
                let source = event.source();
                let mut frame = AxisFrame::new(event.time_msec()).source(source);

                for axis in [Axis::Horizontal, Axis::Vertical] {
                    frame = frame.relative_direction(axis, event.relative_direction(axis));
                    let v120 = event.amount_v120(axis);
                    let value = event
                        .amount(axis)
                        .or_else(|| v120.map(|v| v * 15.0 / 120.0)); // wheel clicks to logical px
                    match value {
                        Some(v) if v != 0.0 => {
                            frame = frame.value(axis, v);
                            if let Some(v120) = v120 {
                                frame = frame.v120(axis, v120.round() as i32);
                            }
                        }
                        // Finger source with an explicit zero means the fingers lifted.
                        Some(_) if source == AxisSource::Finger => frame = frame.stop(axis),
                        _ => {}
                    }
                }

                let pointer = state.seat_state.pointer.clone();
                pointer.axis(state, frame);
                pointer.frame(state);
            }
            InputEvent::PointerButton { event } => {
                let pointer = state.seat_state.pointer.clone();
                pointer.button(
                    state,
                    &ButtonEvent {
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                        button: event.button_code(),
                        state: event.state(),
                    },
                );
                pointer.frame(state);
            }
            InputEvent::PointerMotionAbsolute { event } => {
                if !locked {
                    let position = state
                        .output
                        .current_mode()
                        .map(|x| x.size.to_logical(1))
                        .map(|size| event.position_transformed(size));
                    let focus = state
                        .surface
                        .as_ref()
                        .and_then(|x| x.wl_surface())
                        .map(|x| (x.into_owned(), (0., 0.).into()));
                    let position = position
                        .zip(state.surface.as_ref())
                        .map(|(pos, w)| pos + w.geometry().loc.to_f64());
                    if let Some(position) = position {
                        let pointer = state.seat_state.pointer.clone();
                        println!("{position:?}");
                        pointer.motion(
                            state,
                            focus,
                            &MotionEvent {
                                location: position,
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time_msec(),
                            },
                        );
                        pointer.frame(state);
                        state.redraw.ping();
                    }
                }
            }
            InputEvent::PointerMotion { event } => {
                let (Some(mode), Some(window)) =
                    (state.output.current_mode(), state.surface.as_ref())
                else {
                    return;
                };
                let cur =
                    state.seat_state.pointer.current_location() - window.geometry().loc.to_f64();

                let pointer = state.seat_state.pointer.clone();
                let focus = state
                    .surface
                    .as_ref()
                    .and_then(|x| x.wl_surface())
                    .map(|x| (x.into_owned(), (0., 0.).into()));

                // Relative motion goes to the focused surface regardless of lock state.
                pointer.relative_motion(
                    state,
                    focus.clone(),
                    &RelativeMotionEvent {
                        delta: event.delta(),
                        delta_unaccel: event.delta_unaccel(),
                        utime: event.time(),
                    },
                );
                if !locked {
                    let next = cur + event.delta();
                    let position = Point::<f64, Logical>::from((
                        next.x.clamp(0., (mode.size.w - 1) as f64),
                        next.y.clamp(0., (mode.size.h - 1) as f64),
                    ));

                    let position = Some(position)
                        .zip(state.surface.as_ref())
                        .map(|(pos, w)| pos + w.geometry().loc.to_f64());
                    if let Some(position) = position {
                        pointer.motion(
                            state,
                            focus,
                            &MotionEvent {
                                location: position,
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time_msec(),
                            },
                        );
                    }
                }
                pointer.frame(state);
                state.redraw.ping();
            }
            _ => {}
        }
    }
}
