use parking_lot::Mutex;
use std::sync::Arc;

use crate::editor::view::{View, ViewId};

/// Wraps a native `Box<dyn View>` so it can participate in the view tree.
pub struct NativeViewHandle {
    pub inner: Arc<Mutex<Box<dyn View + Send>>>,
    pub id: ViewId,
}

impl NativeViewHandle {
    pub fn new(view: Box<dyn View + Send>, id: ViewId) -> Self {
        Self {
            inner: Arc::new(Mutex::new(view)),
            id,
        }
    }

}
