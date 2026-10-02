use std::{os::fd::AsFd, sync::Arc};

use calloop::{EventLoop, Interest, LoopSignal, ping::Ping};
use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    desktop::Window,
    input::{
        Seat, SeatState as WaylandSeatState,
        keyboard::KeyboardHandle,
        pointer::{CursorImageStatus, PointerHandle},
    },
    output::{Output, PhysicalProperties, Subpixel},
    utils::{Logical, Physical, Point, Size},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        cursor_shape::CursorShapeManagerState,
        dmabuf::{DmabufFeedback, DmabufGlobal, DmabufState, ImportNotifier},
        pointer_constraints::PointerConstraintsState,
        relative_pointer::RelativePointerManagerState,
        shell::xdg::{XdgShellState, decoration::XdgDecorationState},
        shm::ShmState,
    },
};
use wayland_server::{
    Display, DisplayHandle, ListeningSocket,
    backend::{ClientData, ClientId, DisconnectReason},
};

use crate::LoopData;

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
    pub _seat: Seat<App>,
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
