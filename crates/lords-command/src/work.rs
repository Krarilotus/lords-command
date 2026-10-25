//! Work that takes longer than a frame (check, sync plans, apply) runs on its own thread; the window polls for
//! the result and repaints when it arrives.

use eframe::egui;
use std::sync::{Arc, Mutex};

pub struct Task<T> {
    slot: Arc<Mutex<Option<T>>>,
}

impl<T: Send + 'static> Task<T> {
    pub fn start(ctx: &egui::Context, work: impl FnOnce() -> T + Send + 'static) -> Task<T> {
        let slot = Arc::new(Mutex::new(None));
        let (to, ctx) = (slot.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = work();
            *to.lock().expect("task slot") = Some(result);
            ctx.request_repaint();
        });
        Task { slot }
    }

    /// The result, once; `None` while it runs.
    pub fn take(&self) -> Option<T> {
        self.slot.lock().expect("task slot").take()
    }
}
