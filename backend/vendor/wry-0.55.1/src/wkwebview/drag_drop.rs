// Copyright 2020-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

use std::{
  ffi::CStr,
  panic::{catch_unwind, AssertUnwindSafe},
  path::PathBuf,
};

use objc2::{
  runtime::{Bool, ProtocolObject},
  DeclaredClass,
};
use objc2_app_kit::{NSDragOperation, NSDraggingInfo, NSFilenamesPboardType};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSString};

use crate::DragDropEvent;

use super::WryWebView;

// Anything inside the four `NSDraggingDestination` callbacks runs across the
// ObjC↔Rust boundary. If a panic escapes (from the user listener, from a
// surprise nil/objc method, from a poisoned mutex inside the Tauri-supplied
// closure, etc.), the unwind is undefined behaviour and the whole process
// aborts. That's exactly what's happening when a file is dragged out of the
// macOS file picker: the picker's pasteboard format isn't the legacy
// NSFilenamesPboardType, the collect_paths path early-returns fine, but the
// listener or one of the super-class msg_send paths still panics on the
// unusual drag-info shape and kills the app.
//
// Wrap each handler in catch_unwind with a safe default. Once we cross back
// into Rust frames the panic dies harmlessly; if anything inside misbehaves
// the user sees a no-op drag instead of a crash.
fn safe_call<R>(label: &str, default: R, f: impl FnOnce() -> R) -> R {
  match catch_unwind(AssertUnwindSafe(f)) {
    Ok(value) => value,
    Err(_) => {
      // Swallowing the panic here is deliberate — we can't propagate, and we
      // have no Tauri logger guaranteed to be safe at this point. Stderr is
      // the least-bad thing.
      eprintln!("wry drag-drop: handler `{label}` panicked; suppressing to keep app alive");
      default
    }
  }
}

pub(crate) unsafe fn collect_paths(drag_info: &ProtocolObject<dyn NSDraggingInfo>) -> Vec<PathBuf> {
  let pb = drag_info.draggingPasteboard();
  let mut drag_drop_paths = Vec::new();
  let types = NSArray::arrayWithObject(NSFilenamesPboardType);

  if pb.availableTypeFromArray(&types).is_some() {
    let Some(paths) = pb.propertyListForType(NSFilenamesPboardType) else {
      return drag_drop_paths;
    };
    let Ok(paths) = paths.downcast::<NSArray>() else {
      return drag_drop_paths;
    };
    for path in paths {
      let Ok(path) = path.downcast::<NSString>() else {
        continue;
      };
      let raw = path.UTF8String();
      if raw.is_null() {
        continue;
      }
      let path = CStr::from_ptr(raw).to_string_lossy();
      drag_drop_paths.push(PathBuf::from(path.into_owned()));
    }
  }
  drag_drop_paths
}

pub(crate) fn dragging_entered(
  this: &WryWebView,
  drag_info: &ProtocolObject<dyn NSDraggingInfo>,
) -> NSDragOperation {
  safe_call("draggingEntered", NSDragOperation::None, || {
    let paths = unsafe { collect_paths(drag_info) };
    let dl: NSPoint = unsafe { drag_info.draggingLocation() };
    let frame: NSRect = this.frame();
    let position = (dl.x as i32, (frame.size.height - dl.y) as i32);

    let listener = &this.ivars().drag_drop_handler;
    if !listener(DragDropEvent::Enter { paths, position }) {
      // Reject the Wry file drop (invoke the OS default behaviour)
      unsafe { objc2::msg_send![super(this), draggingEntered: drag_info] }
    } else {
      NSDragOperation::Copy
    }
  })
}

pub(crate) fn dragging_updated(
  this: &WryWebView,
  drag_info: &ProtocolObject<dyn NSDraggingInfo>,
) -> NSDragOperation {
  safe_call("draggingUpdated", NSDragOperation::None, || {
    let dl: NSPoint = unsafe { drag_info.draggingLocation() };
    let frame: NSRect = this.frame();
    let position = (dl.x as i32, (frame.size.height - dl.y) as i32);

    let listener = &this.ivars().drag_drop_handler;
    if !listener(DragDropEvent::Over { position }) {
      unsafe {
        let os_operation = objc2::msg_send![super(this), draggingUpdated: drag_info];
        if os_operation == NSDragOperation::None {
          // 0 will be returned for a drop on any arbitrary location on the webview.
          // We'll override that with NSDragOperationCopy.
          NSDragOperation::Copy
        } else {
          // A different NSDragOperation is returned when a file is hovered over something like
          // a <input type="file">, so we'll make sure to preserve that behaviour.
          os_operation
        }
      }
    } else {
      NSDragOperation::Copy
    }
  })
}

pub(crate) fn perform_drag_operation(
  this: &WryWebView,
  drag_info: &ProtocolObject<dyn NSDraggingInfo>,
) -> Bool {
  safe_call("performDragOperation", Bool::NO, || {
    let paths = unsafe { collect_paths(drag_info) };
    let dl: NSPoint = unsafe { drag_info.draggingLocation() };
    let frame: NSRect = this.frame();
    let position = (dl.x as i32, (frame.size.height - dl.y) as i32);

    let listener = &this.ivars().drag_drop_handler;
    if !listener(DragDropEvent::Drop { paths, position }) {
      // Reject the Wry drop (invoke the OS default behaviour)
      unsafe { objc2::msg_send![super(this), performDragOperation: drag_info] }
    } else {
      Bool::YES
    }
  })
}

pub(crate) fn dragging_exited(this: &WryWebView, drag_info: &ProtocolObject<dyn NSDraggingInfo>) {
  safe_call("draggingExited", (), || {
    let listener = &this.ivars().drag_drop_handler;
    if !listener(DragDropEvent::Leave) {
      // Reject the Wry drop (invoke the OS default behaviour)
      unsafe { objc2::msg_send![super(this), draggingExited: drag_info] }
    }
  })
}
