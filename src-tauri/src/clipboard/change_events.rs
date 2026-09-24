/// OS clipboard change notifications that wake the polling monitors early.
///
/// The X11/Windows/macOS monitor (plugin.rs) and the wlroots monitor
/// (wayland.rs) used to check the clipboard every 500 ms, so a copy waited up
/// to half a second before it was even noticed (issue #20). Each listener here
/// blocks on the platform's own change event and sends `()` down a channel;
/// the monitor loop waits on that channel via `wait_for_change` instead of
/// sleeping, reading as soon as something changes.
///
/// The listeners only say "something changed". The monitors still do the
/// actual read and the `should_process_content` echo/dedupe check, and still
/// time out and poll on their old interval. A listener that fails to start or
/// dies just leaves its monitor polling exactly as before.
///
/// macOS has no clipboard change notification; its monitor polls the cheap
/// NSPasteboard changeCount at a shorter interval instead (see plugin.rs).
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::sync::mpsc::Sender;

/// Pause after a change event before the monitor reads. A single copy can
/// fire several events in quick succession (Windows apps that write formats
/// in separate clipboard opens, clipboard managers re-owning the selection),
/// and reading at the first one can catch a half-written clipboard or contend
/// with the source app's own OpenClipboard. 30 ms is well under what a person
/// notices and long enough to fold a burst into one read.
const SETTLE: Duration = Duration::from_millis(30);

/// Block until a change event arrives on `wake` or `timeout` elapses. When
/// woken, waits `SETTLE` and drains any events that queued up meanwhile, so a
/// burst costs one read. If every sender is gone (listener never started or
/// died), sleeps the full `timeout` so the caller degrades to plain polling
/// rather than spinning.
pub fn wait_for_change(wake: &Receiver<()>, timeout: Duration) {
    match wake.recv_timeout(timeout) {
        Ok(()) => {
            thread::sleep(SETTLE);
            while wake.try_recv().is_ok() {}
        }
        Err(RecvTimeoutError::Timeout) => {}
        Err(RecvTimeoutError::Disconnected) => thread::sleep(timeout),
    }
}

/// Watch X11 CLIPBOARD ownership changes via XFixes and signal `wake` on each.
/// Every copy on X11 is an app claiming ownership of the CLIPBOARD selection,
/// so SetSelectionOwner is the change event; owner-window destruction and
/// client exit are included because they can empty the clipboard.
#[cfg(target_os = "linux")]
pub fn spawn_x11_listener(wake: Sender<()>) {
    thread::spawn(move || {
        if let Err(e) = run_x11_listener(&wake) {
            tracing::warn!(
                "X11 clipboard change listener stopped ({}); clipboard monitor falls back to polling",
                e
            );
        }
    });
}

#[cfg(target_os = "linux")]
fn run_x11_listener(wake: &Sender<()>) -> Result<(), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
    use x11rb::protocol::xproto::ConnectionExt as _;
    use x11rb::protocol::Event;

    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    // XFixes requests are only valid after the version has been negotiated.
    conn.xfixes_query_version(5, 0)?.reply()?;
    let clipboard = conn.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
    conn.xfixes_select_selection_input(
        root,
        clipboard,
        SelectionEventMask::SET_SELECTION_OWNER
            | SelectionEventMask::SELECTION_WINDOW_DESTROY
            | SelectionEventMask::SELECTION_CLIENT_CLOSE,
    )?
    .check()?;
    tracing::info!("X11 clipboard change listener started (XFixes)");

    loop {
        if let Event::XfixesSelectionNotify(_) = conn.wait_for_event()? {
            if wake.send(()).is_err() {
                // Monitor thread has exited.
                return Ok(());
            }
        }
    }
}

/// Watch WM_CLIPBOARDUPDATE (AddClipboardFormatListener) and signal `wake` on
/// each. Windows posts it after the writer closes the clipboard, so the new
/// content is complete by the time the monitor reads.
#[cfg(target_os = "windows")]
pub fn spawn_windows_listener(wake: Sender<()>) {
    thread::spawn(move || {
        // The monitor's message-only window belongs to the thread that creates
        // it, so it has to be built on the thread that waits on it.
        let mut monitor = match clipboard_win::monitor::Monitor::new() {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    "Windows clipboard change listener failed to start ({}); clipboard monitor falls back to polling",
                    e
                );
                return;
            }
        };
        tracing::info!("Windows clipboard change listener started (WM_CLIPBOARDUPDATE)");

        loop {
            match monitor.recv() {
                Ok(true) => {
                    if wake.send(()).is_err() {
                        return;
                    }
                }
                // Shutdown requested via the monitor's shutdown channel.
                Ok(false) => return,
                Err(e) => {
                    tracing::warn!(
                        "Windows clipboard change listener stopped ({}); clipboard monitor falls back to polling",
                        e
                    );
                    return;
                }
            }
        }
    });
}

/// Watch the Wayland selection via ext-data-control / wlr-data-control and
/// signal `wake` each time it changes. Same protocol preference as
/// wl-clipboard-rs, so it watches the selection the monitor reads through.
#[cfg(target_os = "linux")]
pub fn spawn_data_control_listener(wake: Sender<()>) {
    thread::spawn(move || {
        if let Err(e) = data_control::run(wake) {
            tracing::warn!(
                "Wayland clipboard change listener stopped ({}); clipboard monitor falls back to polling",
                e
            );
        }
    });
}

#[cfg(target_os = "linux")]
mod data_control {
    use std::sync::mpsc::Sender;
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::wl_registry::{self, WlRegistry};
    use wayland_client::protocol::wl_seat::WlSeat;
    use wayland_client::{delegate_noop, event_created_child, Connection, Dispatch, QueueHandle};
    use wayland_protocols::ext::data_control::v1::client::{
        ext_data_control_device_v1::{self as ext_device, ExtDataControlDeviceV1},
        ext_data_control_manager_v1::ExtDataControlManagerV1,
        ext_data_control_offer_v1::ExtDataControlOfferV1,
    };
    use wayland_protocols_wlr::data_control::v1::client::{
        zwlr_data_control_device_v1::{self as wlr_device, ZwlrDataControlDeviceV1},
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
        zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    };

    struct Listener {
        wake: Sender<()>,
        /// Set when the monitor has exited or the compositor retired the device.
        done: bool,
    }

    impl Listener {
        fn selection_changed(&mut self) {
            if self.wake.send(()).is_err() {
                self.done = true;
            }
        }
    }

    impl Dispatch<WlRegistry, GlobalListContents> for Listener {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    delegate_noop!(Listener: ignore WlSeat);
    delegate_noop!(Listener: ExtDataControlManagerV1);
    delegate_noop!(Listener: ZwlrDataControlManagerV1);
    // Offers only announce MIME types; the monitor reads the content itself
    // through wl-clipboard-rs.
    delegate_noop!(Listener: ignore ExtDataControlOfferV1);
    delegate_noop!(Listener: ignore ZwlrDataControlOfferV1);

    // Each selection arrives as a fresh offer object. The listener never reads
    // from it, so it's destroyed straight away; otherwise the compositor keeps
    // every past selection's offer alive until we disconnect.
    impl Dispatch<ExtDataControlDeviceV1, ()> for Listener {
        fn event(
            state: &mut Self,
            device: &ExtDataControlDeviceV1,
            event: ext_device::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                ext_device::Event::Selection { id } => {
                    if let Some(offer) = id {
                        offer.destroy();
                    }
                    state.selection_changed();
                }
                ext_device::Event::PrimarySelection { id } => {
                    if let Some(offer) = id {
                        offer.destroy();
                    }
                }
                ext_device::Event::Finished => {
                    device.destroy();
                    state.done = true;
                }
                _ => {}
            }
        }

        event_created_child!(Listener, ExtDataControlDeviceV1, [
            ext_device::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
        ]);
    }

    impl Dispatch<ZwlrDataControlDeviceV1, ()> for Listener {
        fn event(
            state: &mut Self,
            device: &ZwlrDataControlDeviceV1,
            event: wlr_device::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                wlr_device::Event::Selection { id } => {
                    if let Some(offer) = id {
                        offer.destroy();
                    }
                    state.selection_changed();
                }
                // Not sent at the v1 we bind, but harmless to handle.
                wlr_device::Event::PrimarySelection { id } => {
                    if let Some(offer) = id {
                        offer.destroy();
                    }
                }
                wlr_device::Event::Finished => {
                    device.destroy();
                    state.done = true;
                }
                _ => {}
            }
        }

        event_created_child!(Listener, ZwlrDataControlDeviceV1, [
            wlr_device::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
        ]);
    }

    pub fn run(wake: Sender<()>) -> Result<(), Box<dyn std::error::Error>> {
        let conn = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<Listener>(&conn)?;
        let qh = queue.handle();

        // First seat only, matching wl-clipboard-rs's `Seat::Unspecified`.
        let seat: WlSeat = globals.bind(&qh, 1..=1, ())?;
        if let Ok(manager) = globals.bind::<ExtDataControlManagerV1, _, _>(&qh, 1..=1, ()) {
            manager.get_data_device(&seat, &qh, ());
        } else {
            // v1 has no primary-selection events, which we don't watch anyway.
            let manager: ZwlrDataControlManagerV1 = globals.bind(&qh, 1..=1, ())?;
            manager.get_data_device(&seat, &qh, ());
        }
        tracing::info!("Wayland clipboard change listener started (data-control)");

        let mut listener = Listener { wake, done: false };
        while !listener.done {
            queue.blocking_dispatch(&mut listener)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    #[test]
    fn returns_early_when_woken() {
        let (tx, rx) = mpsc::channel();
        tx.send(()).unwrap();
        let start = Instant::now();
        wait_for_change(&rx, Duration::from_secs(5));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_burst_of_events_costs_one_wake() {
        let (tx, rx) = mpsc::channel();
        for _ in 0..3 {
            tx.send(()).unwrap();
        }
        wait_for_change(&rx, Duration::from_secs(5));
        assert!(rx.try_recv().is_err(), "queued events should be drained");
    }

    #[test]
    fn times_out_without_an_event() {
        let (_tx, rx) = mpsc::channel::<()>();
        let start = Instant::now();
        wait_for_change(&rx, Duration::from_millis(50));
        assert!(start.elapsed() >= Duration::from_millis(50));
    }

    #[test]
    fn dead_listener_degrades_to_polling_instead_of_spinning() {
        let (tx, rx) = mpsc::channel::<()>();
        drop(tx);
        let start = Instant::now();
        wait_for_change(&rx, Duration::from_millis(50));
        assert!(start.elapsed() >= Duration::from_millis(50));
    }
}
