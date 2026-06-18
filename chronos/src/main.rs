//! Chronos — a fullscreen wlr-layer-shell clock overlay for SHQ kiosks.
//!
//! Spawned by `nyx` when a kiosk in `idle_mode: clock` times out. It paints a
//! large two-line portrait clock (HH over MM) on the **overlay** layer, which
//! by protocol sits above the fullscreen Chromium kiosk — so it hides the
//! dashboard without touching Chrome. On wake, nyx kills the process; the
//! surface is torn down and the live dashboard is revealed instantly, exactly
//! where the user left it.
//!
//! Chronos handles no input — `nyx` grabs the evdev touch device while the
//! screensaver is up, so the wake tap never reaches the compositor.

mod render;

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{Local, Timelike};
use smithay_client_toolkit::reexports::calloop::{
    timer::{TimeoutAction, Timer},
    EventLoop,
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use tracing_subscriber::EnvFilter;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

/// Opaque black background and a soft white foreground (ARGB8888, 0xAARRGGBB).
const BG: u32 = 0xFF00_0000;
const FG: u32 = 0xFFE6_E6E6;

struct Chronos {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: Option<SlotPool>,
    layer: LayerSurface,
    font: fontdue::Font,
    width: u32,
    height: u32,
    configured: bool,
    exit: bool,
    last: Option<(u8, u8)>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "chronos=info".into()),
        )
        .init();

    let conn = Connection::connect_to_env()
        .context("failed to connect to the Wayland compositor (WAYLAND_DISPLAY set?)")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .context("wl_compositor not available")?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .context("wlr-layer-shell not available")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm not available")?;

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some("chronos"), None);
    layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_exclusive_zone(-1); // ignore panels' exclusive zones; cover everything
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_size(0, 0); // anchored to all edges → compositor gives us the full output
    layer.commit();

    let mut state = Chronos {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool: None,
        layer,
        font: render::load_font(),
        width: 0,
        height: 0,
        configured: false,
        exit: false,
        last: None,
    };

    let mut event_loop: EventLoop<Chronos> =
        EventLoop::try_new().context("failed to create event loop")?;
    let handle = event_loop.handle();

    WaylandSource::new(conn.clone(), event_queue)
        .insert(handle.clone())
        .map_err(|e| anyhow!("failed to insert wayland source: {e}"))?;

    let timer_qh = qh.clone();
    handle
        .insert_source(Timer::immediate(), move |_deadline, _, state: &mut Chronos| {
            state.tick(&timer_qh);
            TimeoutAction::ToDuration(Duration::from_secs(1))
        })
        .map_err(|e| anyhow!("failed to insert timer: {e}"))?;

    tracing::info!("Chronos v{} starting", env!("CARGO_PKG_VERSION"));
    while !state.exit {
        event_loop
            .dispatch(Duration::from_secs(1), &mut state)
            .context("event loop dispatch failed")?;
    }
    Ok(())
}

impl Chronos {
    /// Once-per-second tick: redraw only when the displayed minute changes.
    fn tick(&mut self, qh: &QueueHandle<Self>) {
        if !self.configured {
            return;
        }
        let now = Local::now();
        let hm = (now.hour() as u8, now.minute() as u8);
        if self.last == Some(hm) {
            return;
        }
        self.last = Some(hm);
        self.draw(qh);
    }

    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        let (w, h) = (self.width as i32, self.height as i32);
        if w == 0 || h == 0 {
            return;
        }
        let (hh, mm) = self.last.unwrap_or((0, 0));
        let stride = w * 4;

        let pool = match self.pool.as_mut() {
            Some(p) => p,
            None => return,
        };
        let (buffer, bytes) = match pool.create_buffer(w, h, stride, wl_shm::Format::Argb8888) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("create_buffer failed: {e}");
                return;
            }
        };

        // The shm slot is allocated at a 4-byte-aligned offset (slot sizes are
        // multiples of 4), so reinterpreting the byte canvas as u32 is sound.
        let px: &mut [u32] = unsafe {
            std::slice::from_raw_parts_mut(bytes.as_mut_ptr() as *mut u32, bytes.len() / 4)
        };
        let mut canvas = render::Canvas {
            buf: px,
            width: w,
            height: h,
        };
        render::render_clock(&mut canvas, &self.font, hh, mm, FG, BG);

        let surface = self.layer.wl_surface();
        surface.attach(Some(buffer.wl_buffer()), 0, 0);
        surface.damage_buffer(0, 0, w, h);
        surface.commit();
    }
}

impl CompositorHandler for Chronos {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }
    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Chronos {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for Chronos {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let (w, h) = configure.new_size;
        if w != 0 && h != 0 {
            self.width = w;
            self.height = h;
        }
        if self.pool.is_none() && self.width > 0 && self.height > 0 {
            let len = (self.width * self.height * 4) as usize;
            match SlotPool::new(len, &self.shm) {
                Ok(p) => self.pool = Some(p),
                Err(e) => {
                    tracing::error!("failed to create shm pool: {e}");
                    return;
                }
            }
            tracing::info!("configured at {}x{}", self.width, self.height);
        }
        self.configured = true;
        self.last = None; // force a repaint after (re)configure
        self.tick(qh);
    }
}

impl ShmHandler for Chronos {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Chronos {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(Chronos);
delegate_output!(Chronos);
delegate_shm!(Chronos);
delegate_layer!(Chronos);
delegate_registry!(Chronos);
