//! Background work → GTK main loop.

use gtk::glib;

/// Run `work` on a thread; call `done` on the GTK main context when it finishes.
pub fn run<T, F, D>(work: F, done: D)
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    D: FnOnce(T) + 'static,
{
    let (tx, rx) = async_channel::bounded::<T>(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(work());
    });
    glib::MainContext::default().spawn_local(async move {
        if let Ok(value) = rx.recv().await {
            done(value);
        }
    });
}
