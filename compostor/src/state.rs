use std::{os::fd::AsFd, process::Command, sync::Arc};

use calloop::{
    EventLoop, Interest, LoopHandle, LoopSignal,
    ping::{Ping, PingSource},
};
use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        input::{
            AbsolutePositionEvent, Axis, AxisSource, Event, InputBackend, InputEvent, KeyState,
            KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
        },
        session::Session,
    },
    desktop::Window,
    input::{
        Seat, SeatState as WaylandSeatState,
        keyboard::{FilterResult, KeyboardHandle, Keysym},
        pointer::{
            AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent, PointerHandle,
            RelativeMotionEvent,
        },
    },
    output::{Output, PhysicalProperties, Subpixel},
    utils::{Logical, Physical, Point, SERIAL_COUNTER, Size},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufFeedback, DmabufGlobal, DmabufState, ImportNotifier},
        pointer_constraints::PointerConstraintsState,
        relative_pointer::RelativePointerManagerState,
        seat::WaylandFocus,
        shell::xdg::{XdgShellState, decoration::XdgDecorationState},
        shm::ShmState,
    },
};
use wayland_server::{
    Display, DisplayHandle, ListeningSocket,
    backend::{ClientData, ClientId, DisconnectReason},
};

use crate::{LoopData, udev::UdevBackend, winit::WinitBackend};

#[derive(Debug)]
pub struct App {
    pub compositor_state: CompositorState,
    pub seat_state: SeatState,
    pub shm_state: ShmState,
    pub _pointer_constraint_state: PointerConstraintsState,
    pub _relative_pointer_state: RelativePointerManagerState,
    pub _cursor_state: CursorShapeManagerState,
    pub xdg_shell_state: XdgShellState,
    pub _xdg_decoration: XdgDecorationState,
    pub listener: ListeningSocket,
    pub loop_signal: Option<LoopSignal>,
    pub output: Output,
    pub surface: Option<Window>,
    pub redraw: Ping,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub pending_dmabufs: Vec<(Dmabuf, ImportNotifier)>,
}
impl App {
    pub fn new(
        dh: &DisplayHandle,
        redraw: Ping,
        feedback: Option<&DmabufFeedback>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let listener = ListeningSocket::bind("wayland-5")?;

        let compositor_state = CompositorState::new::<App>(dh);
        let shm_state = ShmState::new::<App>(dh, []);
        let _pointer_constraint_state = PointerConstraintsState::new::<App>(dh);
        let _relative_pointer_state = RelativePointerManagerState::new::<App>(dh);
        let xdg_shell_state = XdgShellState::new::<App>(dh);
        let _xdg_decoration = XdgDecorationState::new::<App>(dh);
        let _cursor_state = CursorShapeManagerState::new::<App>(dh);
        let mut dmabuf_state = DmabufState::new();
        let dmabuf_global =
            feedback.map(|f| dmabuf_state.create_global_with_default_feedback::<App>(dh, f));
        let output = smithay::output::Output::new(
            "output-0".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "MyCompositor".into(),
                model: "Virtual Display".into(),
            },
        );
        output.change_current_state(
            None,
            Some(smithay::utils::Transform::Normal),
            Some(smithay::output::Scale::Integer(1)),
            None,
        );
        output.create_global::<App>(dh);
        Ok(Self {
            compositor_state,
            seat_state: SeatState::new(dh)?,
            shm_state,
            _pointer_constraint_state,
            _relative_pointer_state,
            _cursor_state,
            xdg_shell_state,
            _xdg_decoration,
            listener,
            output,
            redraw,
            loop_signal: None,
            surface: None,
            dmabuf_state,
            dmabuf_global,
            pending_dmabufs: Vec::new(),
        })
    }
    pub fn register_loop(
        &mut self,
        event_loop: &EventLoop<LoopData>,
        display: &mut Display<App>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.loop_signal = Some(event_loop.get_signal());

        let listener_fd = self.listener.as_fd().try_clone_to_owned()?;
        let display_poll_fd = display.backend().poll_fd().try_clone_to_owned()?;
        let handle = event_loop.handle();

        handle.insert_source(
            calloop::generic::Generic::new(display_poll_fd, Interest::READ, calloop::Mode::Level),
            |_, _, loop_data| {
                loop_data.display.dispatch_clients(&mut loop_data.app)?;
                Ok(calloop::PostAction::Continue)
            },
        )?;
        handle.insert_source(
            calloop::generic::Generic::new(listener_fd, Interest::READ, calloop::Mode::Level),
            |_, _, loop_data| {
                if let Some(stream) = loop_data.app.listener.accept().unwrap() {
                    loop_data
                        .display
                        .handle()
                        .insert_client(stream, Arc::new(ClientState::default()))?;
                }
                Ok(calloop::PostAction::Continue)
            },
        )?;
        Ok(())
    }
    pub fn exit(&self) {
        if let Some(loop_signal) = self.loop_signal.as_ref() {
            loop_signal.stop();
        } else {
            std::process::exit(0);
        }
    }
    pub fn resize(&self, size: Size<i32, Physical>) {
        self.output.change_current_state(
            Some(smithay::output::Mode {
                size,
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        if let Some(window) = self.surface.as_ref()
            && let Some(surface) = window.toplevel()
        {
            surface.with_pending_state(|surface_state| {
                let position = self.output.current_mode().unwrap().size.to_logical(1);
                surface_state.size = Some(position);
            });
            surface.send_configure();
        }
    }
}
#[derive(Debug)]
pub struct SeatState {
    pub wayland_seat_state: WaylandSeatState<App>,
    pub seat: Seat<App>,
    pub keyboard: KeyboardHandle<App>,
    pub pointer: PointerHandle<App>,
    pub image_status: CursorImageStatus,
    pub position_hint: Option<Point<f64, Logical>>,
}
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

pub trait WindowingBackend: Sized {
    fn new(request_redraw: PingSource) -> Result<Self, Box<dyn std::error::Error>>;
    fn register_loop(
        &mut self,
        loop_handle: &LoopHandle<'_, LoopData>,
    ) -> Result<(), Box<dyn std::error::Error>>;
    fn redraw(&mut self, app: &mut App) -> Result<(), Box<dyn std::error::Error>>;
    fn cursor(&mut self, app: &mut App) -> Result<(), Box<dyn std::error::Error>>;
    fn request_draw(&mut self) -> Result<(), Box<dyn std::error::Error>>;
}

#[derive(Debug)]
pub enum Backend {
    Winit(Box<WinitBackend>),
    Udev(Box<UdevBackend>),
}
impl Backend {
    pub fn cursor(&mut self, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            Backend::Winit(state) => state.cursor(app),
            Backend::Udev(state) => state.cursor(app),
        }
    }
    pub fn request_draw(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            Backend::Winit(state) => state.request_draw(),
            Backend::Udev(state) => state.request_draw(),
        }
    }
    pub fn new(redraw: PingSource) -> Result<Self, Box<dyn std::error::Error>> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            let winit = WinitBackend::new(redraw)?;
            Ok(Self::Winit(Box::new(winit)))
        } else {
            let udev = UdevBackend::new(redraw)?;
            Ok(Self::Udev(Box::new(udev)))
        }
    }
    pub fn register_loop(
        &mut self,
        event_loop: &EventLoop<LoopData>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let handle = event_loop.handle();
        match self {
            Backend::Winit(state) => state.register_loop(&handle),
            Backend::Udev(state) => state.register_loop(&handle),
        }
    }
    pub fn redraw(&mut self, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            Backend::Winit(state) => state.redraw(app),
            Backend::Udev(state) => state.redraw(app),
        }
    }
    pub fn input<E: InputBackend>(&mut self, state: &mut App, event: InputEvent<E>) {
        enum Action {
            Spawn,
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
                        if mods.alt && sym == Keysym::F4 {
                            return FilterResult::Intercept(Action::Spawn);
                        }
                        FilterResult::Forward
                    },
                ) {
                    Some(Action::Quit) => state.exit(),
                    Some(Action::Spawn) => {
                        let mut child = Command::new("sh")
                            .arg("run.sh")
                            .current_dir("..")
                            .stderr(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .spawn()
                            .unwrap();
                        std::thread::spawn(move || {
                            let _ = child.wait();
                        });
                    }
                    Some(Action::Vt(n)) => {
                        if let Backend::Udev(k) = self {
                            let _ = k.session.change_vt(n);
                        }
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
