use crate::{
    DevicePixels, ForegroundExecutor, ScreenCaptureFrame, ScreenCaptureSource, ScreenCaptureStream,
    ScreenCaptureStreamTermination, ScreenCaptureTerminationCallback, SharedString, SourceMetadata,
    size,
};
use anyhow::{Result, anyhow};
use block::ConcreteBlock;
use block2::RcBlock;
use collections::HashMap;
use core_foundation::base::TCFType;
use core_graphics::display::{
    CGDirectDisplayID, CGDisplayCopyDisplayMode, CGDisplayModeGetPixelHeight,
    CGDisplayModeGetPixelWidth, CGDisplayModeRelease,
};
use ctor::ctor;
use futures::channel::oneshot;
use media::core_media::{CMSampleBuffer, CMSampleBufferRef};
use metal::NSInteger;
use objc::{
    class,
    declare::ClassDecl,
    msg_send,
    runtime::{Class, Object, Sel},
    sel, sel_impl,
};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::NSScreen;
use objc2_foundation::{NSError, NSNumber, NSString};
use objc2_screen_capture_kit::{SCDisplay, SCShareableContent};
use std::{
    cell::RefCell,
    ffi::c_void,
    ptr,
    rc::Rc,
    sync::{LazyLock, Mutex},
};

use super::NSStringExt;

type ObjcId = *mut Object;
type FrameCallback = Box<dyn Fn(ScreenCaptureFrame) + Send>;

#[allow(non_camel_case_types)]
type id = ObjcId;

#[allow(non_upper_case_globals)]
const nil: ObjcId = ptr::null_mut();

#[derive(Clone)]
pub struct MacScreenCaptureSource {
    sc_display: Retained<SCDisplay>,
    meta: Option<ScreenMeta>,
}

pub struct MacScreenCaptureStream {
    sc_stream: id,
    sc_stream_output: id,
    meta: SourceMetadata,
}

static mut DELEGATE_CLASS: *const Class = ptr::null();
static mut OUTPUT_CLASS: *const Class = ptr::null();
const FRAME_CALLBACK_IVAR: &str = "frame_callback";

type StreamTerminationCallbacks = HashMap<usize, ScreenCaptureTerminationCallback>;

static STREAM_TERMINATION_CALLBACKS: LazyLock<Mutex<StreamTerminationCallbacks>> =
    LazyLock::new(|| Mutex::new(HashMap::default()));

fn register_stream_termination_callback(stream: id, callback: ScreenCaptureTerminationCallback) {
    let previous = STREAM_TERMINATION_CALLBACKS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(stream as usize, callback);
    if let Some(previous) = previous {
        previous(ScreenCaptureStreamTermination::Cancelled);
    }
}

fn take_stream_termination_callback(stream: id) -> Option<ScreenCaptureTerminationCallback> {
    STREAM_TERMINATION_CALLBACKS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&(stream as usize))
}

fn notify_stream_termination(stream: id, termination: ScreenCaptureStreamTermination) {
    if let Some(callback) = take_stream_termination_callback(stream) {
        callback(termination);
    }
}

#[allow(non_upper_case_globals)]
const SCStreamOutputTypeScreen: NSInteger = 0;
// `SCStreamErrorUserStopped` from ScreenCaptureKit/SCError.h.
const SC_STREAM_ERROR_USER_STOPPED: NSInteger = -3817;

fn stream_termination_from_error(
    error_code: NSInteger,
    description: SharedString,
) -> ScreenCaptureStreamTermination {
    if error_code == SC_STREAM_ERROR_USER_STOPPED {
        ScreenCaptureStreamTermination::Cancelled
    } else {
        ScreenCaptureStreamTermination::Failed(description)
    }
}

impl ScreenCaptureSource for MacScreenCaptureSource {
    fn metadata(&self) -> Result<SourceMetadata> {
        let (display_id, size) = unsafe {
            // SAFETY: `SCDisplay` is an Objective-C object, so objc2's object pointer has the
            // same ABI as objc 0.2's `id`. This only casts a borrowed pointer; it does not
            // transfer ownership or change the retain count. `self.sc_display` remains alive
            // for the entire message send, and `displayID` does not retain the receiver.
            let sc_display: id = Retained::as_ptr(&self.sc_display).cast_mut().cast();
            let display_id: CGDirectDisplayID = msg_send![sc_display, displayID];
            let display_mode_ref = CGDisplayCopyDisplayMode(display_id);
            let width = CGDisplayModeGetPixelWidth(display_mode_ref);
            let height = CGDisplayModeGetPixelHeight(display_mode_ref);
            CGDisplayModeRelease(display_mode_ref);

            (
                display_id,
                size(DevicePixels(width as i32), DevicePixels(height as i32)),
            )
        };
        let (label, is_main) = self
            .meta
            .clone()
            .map(|meta| (meta.label, meta.is_main))
            .unzip();

        Ok(SourceMetadata {
            id: display_id as u64,
            label,
            is_main,
            resolution: size,
        })
    }

    fn stream(
        &self,
        foreground_executor: &ForegroundExecutor,
        frame_callback: Box<dyn Fn(ScreenCaptureFrame) + Send>,
    ) -> oneshot::Receiver<Result<Box<dyn ScreenCaptureStream>>> {
        self.stream_with_termination(foreground_executor, frame_callback, Box::new(|_| {}))
    }

    fn stream_with_termination(
        &self,
        _foreground_executor: &ForegroundExecutor,
        frame_callback: Box<dyn Fn(ScreenCaptureFrame) + Send>,
        termination_callback: ScreenCaptureTerminationCallback,
    ) -> oneshot::Receiver<Result<Box<dyn ScreenCaptureStream>>> {
        self.start_stream(frame_callback, termination_callback)
    }
}

impl MacScreenCaptureSource {
    fn start_stream(
        &self,
        frame_callback: Box<dyn Fn(ScreenCaptureFrame) + Send>,
        termination_callback: ScreenCaptureTerminationCallback,
    ) -> oneshot::Receiver<Result<Box<dyn ScreenCaptureStream>>> {
        unsafe {
            // SAFETY: `SCDisplay` is an Objective-C object, so objc2's object pointer has the
            // same ABI as objc 0.2's `id`. The cast removes pointer constness only because the
            // legacy `id` alias is mutable; it does not mutate the display, transfer ownership,
            // or change the retain count. The generated objc2 binding takes `display` as
            // `&SCDisplay`, confirming that this initializer borrows rather than consumes it.
            // `self.sc_display` remains alive through the call, and `SCContentFilter` must retain
            // the display or copy any state that it needs after initialization.
            let sc_display: id = Retained::as_ptr(&self.sc_display).cast_mut().cast();
            let stream: id = msg_send![class!(SCStream), alloc];
            let filter: id = msg_send![class!(SCContentFilter), alloc];
            let configuration: id = msg_send![class!(SCStreamConfiguration), alloc];
            let delegate: id = msg_send![DELEGATE_CLASS, alloc];
            let output: id = msg_send![OUTPUT_CLASS, alloc];

            let excluded_windows: id = msg_send![class!(NSArray), array];
            let filter: id =
                msg_send![filter, initWithDisplay:sc_display excludingWindows:excluded_windows];
            let configuration: id = msg_send![configuration, init];
            let _: id = msg_send![configuration, setScalesToFit: true];
            let _: id = msg_send![configuration, setPixelFormat: 0x42475241];
            // let _: id = msg_send![configuration, setShowsCursor: false];
            // let _: id = msg_send![configuration, setCaptureResolution: 3];
            let delegate: id = msg_send![delegate, init];
            let output: id = msg_send![output, init];

            output.as_mut().unwrap().set_ivar(
                FRAME_CALLBACK_IVAR,
                Box::into_raw(Box::new(frame_callback)) as *mut c_void,
            );

            let meta = self.metadata().unwrap();
            let _: id = msg_send![configuration, setWidth: meta.resolution.width.0 as i64];
            let _: id = msg_send![configuration, setHeight: meta.resolution.height.0 as i64];
            let stream: id = msg_send![stream, initWithFilter:filter configuration:configuration delegate:delegate];

            // `SCStream` retains these objects for its own lifetime.
            let _: () = msg_send![filter, release];
            let _: () = msg_send![configuration, release];
            let _: () = msg_send![delegate, release];

            let (mut tx, rx) = oneshot::channel();

            let mut error: id = nil;
            let _: () = msg_send![stream, addStreamOutput:output type:SCStreamOutputTypeScreen sampleHandlerQueue:0 error:&mut error as *mut id];
            if error != nil {
                let message: id = msg_send![error, localizedDescription];
                let _: () = msg_send![stream, release];
                let _: () = msg_send![output, release];
                tx.send(Err(anyhow!("failed to add stream output {message:?}")))
                    .ok();
                return rx;
            }

            register_stream_termination_callback(stream, termination_callback);

            let tx = Rc::new(RefCell::new(Some(tx)));
            let handler = ConcreteBlock::new({
                move |error: id| {
                    let result = if error == nil {
                        let stream = MacScreenCaptureStream {
                            meta: meta.clone(),
                            sc_stream: stream,
                            sc_stream_output: output,
                        };
                        Ok(Box::new(stream) as Box<dyn ScreenCaptureStream>)
                    } else {
                        take_stream_termination_callback(stream);
                        let _: () = msg_send![stream, release];
                        let _: () = msg_send![output, release];
                        let message: id = msg_send![error, localizedDescription];
                        Err(anyhow!("failed to start screen capture stream {message:?}"))
                    };
                    if let Some(tx) = tx.borrow_mut().take() {
                        tx.send(result).ok();
                    }
                }
            });
            let handler = handler.copy();
            let _: () = msg_send![stream, startCaptureWithCompletionHandler:handler];
            rx
        }
    }
}

impl ScreenCaptureStream for MacScreenCaptureStream {
    fn metadata(&self) -> Result<SourceMetadata> {
        Ok(self.meta.clone())
    }
}

impl Drop for MacScreenCaptureStream {
    fn drop(&mut self) {
        notify_stream_termination(self.sc_stream, ScreenCaptureStreamTermination::Cancelled);

        unsafe {
            let mut error: id = nil;
            let _: () = msg_send![self.sc_stream, removeStreamOutput:self.sc_stream_output type:SCStreamOutputTypeScreen error:&mut error as *mut _];
            if error != nil {
                let message: id = msg_send![error, localizedDescription];
                log::error!("failed to add stream  output {message:?}");
            }

            let handler = ConcreteBlock::new(move |error: id| {
                if error != nil {
                    let message: id = msg_send![error, localizedDescription];
                    log::error!("failed to stop screen capture stream {message:?}");
                }
            });
            let block = handler.copy();
            let _: () = msg_send![self.sc_stream, stopCaptureWithCompletionHandler:block];
            let _: () = msg_send![self.sc_stream, release];
            let _: () = msg_send![self.sc_stream_output, release];
        }
    }
}

#[derive(Clone)]
struct ScreenMeta {
    label: SharedString,
    // Is this the screen with menu bar?
    is_main: bool,
}

fn screen_id_to_human_label(marker: MainThreadMarker) -> HashMap<CGDirectDisplayID, ScreenMeta> {
    let screens = NSScreen::screens(marker);
    let mut map = HashMap::default();

    let screen_number_key = NSString::from_str("NSScreenNumber");

    for (i, screen) in screens.iter().enumerate() {
        let description = screen.deviceDescription();
        let Some(obj) = description.objectForKey(&screen_number_key) else {
            continue;
        };
        let Some(screen_id) = obj.downcast_ref::<NSNumber>() else {
            continue;
        };
        let name = screen.localizedName().to_string();

        map.insert(
            screen_id.as_u32(),
            ScreenMeta {
                label: name.into(),
                is_main: i == 0,
            },
        );
    }

    map
}

pub(crate) fn get_sources(
    marker: MainThreadMarker,
) -> oneshot::Receiver<Result<Vec<Rc<dyn ScreenCaptureSource>>>> {
    let (tx, rx) = oneshot::channel();
    let tx = Rc::new(RefCell::new(Some(tx)));
    let screen_id_to_label = screen_id_to_human_label(marker);

    let handler = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let Some(tx) = tx.borrow_mut().take() else {
                return;
            };

            let result = if let Some(error) = unsafe { error.as_ref() } {
                Err(anyhow!(
                    "Screen share failed: {}",
                    error.localizedDescription()
                ))
            } else if let Some(content) = unsafe { content.as_ref() } {
                // SAFETY: Marked unsafe conservatively by objc2
                let result = unsafe { content.displays() }
                    .into_iter()
                    .map(|display| {
                        // SAFETY: Marked unsafe conservatively by objc2
                        let id = unsafe { display.displayID() };
                        let metadata = screen_id_to_label.get(&id).cloned();
                        let source = MacScreenCaptureSource {
                            sc_display: display,
                            meta: metadata,
                        };
                        Rc::new(source) as Rc<dyn ScreenCaptureSource>
                    })
                    .collect::<Vec<_>>();

                Ok(result)
            } else {
                // The two pointers are mutually exclusive, this should never happen
                Err(anyhow!("Screen share failed"))
            };

            _ = tx.send(result);
        },
    );

    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true, true, &handler);
    }

    rx
}

#[ctor]
unsafe fn build_classes() {
    let mut decl = ClassDecl::new("GPUIStreamDelegate", class!(NSObject)).unwrap();
    unsafe {
        decl.add_method(
            sel!(outputVideoEffectDidStartForStream:),
            output_video_effect_did_start_for_stream as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(outputVideoEffectDidStopForStream:),
            output_video_effect_did_stop_for_stream as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(stream:didStopWithError:),
            stream_did_stop_with_error as extern "C" fn(&Object, Sel, id, id),
        );
        DELEGATE_CLASS = decl.register();

        let mut decl = ClassDecl::new("GPUIStreamOutput", class!(NSObject)).unwrap();
        decl.add_method(
            sel!(dealloc),
            dealloc_stream_output as extern "C" fn(&Object, Sel),
        );
        decl.add_method(
            sel!(stream:didOutputSampleBuffer:ofType:),
            stream_did_output_sample_buffer_of_type
                as extern "C" fn(&Object, Sel, id, id, NSInteger),
        );
        decl.add_ivar::<*mut c_void>(FRAME_CALLBACK_IVAR);

        OUTPUT_CLASS = decl.register();
    }
}

extern "C" fn output_video_effect_did_start_for_stream(_this: &Object, _: Sel, _stream: id) {}

extern "C" fn output_video_effect_did_stop_for_stream(_this: &Object, _: Sel, _stream: id) {}

extern "C" fn stream_did_stop_with_error(_this: &Object, _: Sel, stream: id, error: id) {
    let termination = unsafe {
        if error == nil {
            ScreenCaptureStreamTermination::Ended
        } else {
            let error_code: NSInteger = msg_send![error, code];
            let message: id = msg_send![error, localizedDescription];
            stream_termination_from_error(error_code, NSStringExt::to_str(&message).into())
        }
    };
    notify_stream_termination(stream, termination);
}

unsafe fn drop_stream_output_callback(output: &Object) {
    unsafe {
        let callback = *output.get_ivar::<*mut c_void>(FRAME_CALLBACK_IVAR) as *mut FrameCallback;
        if callback.is_null() {
            return;
        }

        drop(Box::from_raw(callback));
    }
}

extern "C" fn dealloc_stream_output(this: &Object, _: Sel) {
    unsafe {
        drop_stream_output_callback(this);
        let _: () = msg_send![super(this, class!(NSObject)), dealloc];
    }
}

extern "C" fn stream_did_output_sample_buffer_of_type(
    this: &Object,
    _: Sel,
    _stream: id,
    sample_buffer: id,
    buffer_type: NSInteger,
) {
    if buffer_type != SCStreamOutputTypeScreen {
        return;
    }

    unsafe {
        let sample_buffer = sample_buffer as CMSampleBufferRef;
        let sample_buffer = CMSampleBuffer::wrap_under_get_rule(sample_buffer);
        if let Some(buffer) = sample_buffer.image_buffer() {
            let callback =
                &*(*this.get_ivar::<*mut c_void>(FRAME_CALLBACK_IVAR) as *const FrameCallback);
            callback(ScreenCaptureFrame(buffer));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ptr::NonNull, sync::mpsc, time::Duration};

    struct CallbackDropSignal(mpsc::Sender<()>);

    impl Drop for CallbackDropSignal {
        fn drop(&mut self) {
            self.0.send(()).unwrap();
        }
    }

    #[test]
    fn user_stopped_stream_error_is_cancelled() {
        assert_eq!(
            stream_termination_from_error(-3817, "The stream was stopped by the user".into()),
            ScreenCaptureStreamTermination::Cancelled
        );
    }

    #[test]
    fn runtime_stream_error_preserves_its_failure_description() {
        assert_eq!(
            stream_termination_from_error(-3811, "Screen capture failed".into()),
            ScreenCaptureStreamTermination::Failed("Screen capture failed".into())
        );
    }

    #[test]
    fn terminal_notification_is_delivered_once() {
        let stream = NonNull::<Object>::dangling().as_ptr();
        let (sender, receiver) = mpsc::channel();
        register_stream_termination_callback(
            stream,
            Box::new(move |termination| sender.send(termination).unwrap()),
        );

        notify_stream_termination(
            stream,
            ScreenCaptureStreamTermination::Failed("capture device disconnected".into()),
        );
        notify_stream_termination(stream, ScreenCaptureStreamTermination::Ended);

        assert_eq!(
            receiver.recv().unwrap(),
            ScreenCaptureStreamTermination::Failed("capture device disconnected".into())
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn stream_output_dealloc_releases_frame_callback() {
        let (sender, receiver) = mpsc::channel();
        let drop_signal = CallbackDropSignal(sender);
        let callback: FrameCallback = Box::new(move |_| {
            let _ = &drop_signal;
        });

        unsafe {
            let output: id = msg_send![OUTPUT_CLASS, alloc];
            let output: id = msg_send![output, init];
            output.as_mut().unwrap().set_ivar(
                FRAME_CALLBACK_IVAR,
                Box::into_raw(Box::new(callback)) as *mut c_void,
            );
            let _: () = msg_send![output, release];
        }

        assert_eq!(receiver.recv_timeout(Duration::from_millis(100)), Ok(()));
    }
}
