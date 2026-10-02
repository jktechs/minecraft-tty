use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_output, delegate_shm,
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState},
        output::OutputHandler,
        seat::WaylandFocus,
        shm::ShmHandler,
    },
};
use wayland_server::{Client, protocol::wl_surface::WlSurface};

use crate::state::{App, ClientState};

impl CompositorHandler for App {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }
    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<App>(surface);
        self.redraw.ping();
        if let Some(window) = self.surface.as_mut() {
            window.on_commit();
        }
    }
    fn destroyed(&mut self, surface: &WlSurface) {
        if let Some(window) = self.surface.as_ref()
            && let Some(current_surface) = window.wl_surface()
            && *surface == *current_surface
        {
            self.surface = None;
        }
    }
}
impl AsMut<CompositorState> for App {
    fn as_mut(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
}
impl BufferHandler for App {
    fn buffer_destroyed(&mut self, _buffer: &wayland_server::protocol::wl_buffer::WlBuffer) {}
}
impl ShmHandler for App {
    fn shm_state(&self) -> &smithay::wayland::shm::ShmState {
        &self.shm_state
    }
}
impl OutputHandler for App {
    fn output_bound(
        &mut self,
        _output: smithay::output::Output,
        _wl_output: wayland_server::protocol::wl_output::WlOutput,
    ) {
    }
}

delegate_compositor!(App);
delegate_shm!(App);
delegate_output!(App);
