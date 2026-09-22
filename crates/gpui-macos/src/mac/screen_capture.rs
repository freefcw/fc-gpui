use crate::{
    DevicePixels, ForegroundExecutor, ScreenCaptureFrame, ScreenCaptureSource, ScreenCaptureStream,
    ScreenCaptureStreamTermination, ScreenCaptureTerminationCallback, SharedString, SourceMetadata,
    size,
};
use anyhow::{Result, anyhow};
use block2::RcBlock;
use collections::HashMap;
use core_foundation::base::TCFType;
use core_graphics::display::{
    CGDirectDisplayID, CGDisplayCopyDisplayMode, CGDisplayModeGetPixelHeight,
    CGDisplayModeGetPixelWidth, CGDisplayModeRelease,
};
use futures::channel::oneshot;
use media::core_media::CMSampleBuffer;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send, rc::Retained};
use objc2_app_kit::NSScreen;
use objc2_core_media::CMSampleBuffer as Objc2CMSampleBuffer;
use objc2_foundation::{NSArray, NSError, NSNumber, NSObject, NSObjectProtocol, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamErrorCode, SCStreamOutput, SCStreamOutputType, SCWindow,
};
use std::{
    cell::RefCell,
    ptr,
    rc::Rc,
    sync::{LazyLock, Mutex},
};

type FrameCallback = Box<dyn Fn(ScreenCaptureFrame) + Send>;
type StreamKey = usize;
type StreamTerminationCallbacks = HashMap<StreamKey, ScreenCaptureTerminationCallback>;

/// Packed little-endian ARGB8888 (`'BGRA'`), the format ScreenCaptureKit documents
/// as supported for sample buffers backed by an IOSurface.
const PIXEL_FORMAT_BGRA: u32 = 0x4247_5241;

#[derive(Clone)]
pub struct MacScreenCaptureSource {
    sc_display: Retained<SCDisplay>,
    meta: Option<ScreenMeta>,
}

pub struct MacScreenCaptureStream {
    sc_stream: Retained<SCStream>,
    sc_stream_output: Retained<StreamOutput>,
    /// `SCStream`'s delegate is unretained; keep this alive for the stream's
    /// lifetime so `stream:didStopWithError:` can still fire.
    _sc_stream_delegate: Retained<StreamDelegate>,
    meta: SourceMetadata,
}

static STREAM_TERMINATION_CALLBACKS: LazyLock<Mutex<StreamTerminationCallbacks>> =
    LazyLock::new(|| Mutex::new(HashMap::default()));

fn stream_key(stream: &SCStream) -> StreamKey {
    ptr::from_ref(stream) as StreamKey
}

fn register_stream_termination_callback(
    stream: &SCStream,
    callback: ScreenCaptureTerminationCallback,
) {
    register_stream_termination_callback_key(stream_key(stream), callback);
}

fn register_stream_termination_callback_key(
    key: StreamKey,
    callback: ScreenCaptureTerminationCallback,
) {
    let previous = STREAM_TERMINATION_CALLBACKS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(key, callback);
    if let Some(previous) = previous {
        previous(ScreenCaptureStreamTermination::Cancelled);
    }
}

fn take_stream_termination_callback(stream: &SCStream) -> Option<ScreenCaptureTerminationCallback> {
    take_stream_termination_callback_key(stream_key(stream))
}

fn take_stream_termination_callback_key(
    key: StreamKey,
) -> Option<ScreenCaptureTerminationCallback> {
    STREAM_TERMINATION_CALLBACKS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&key)
}

fn notify_stream_termination(stream: &SCStream, termination: ScreenCaptureStreamTermination) {
    if let Some(callback) = take_stream_termination_callback(stream) {
        callback(termination);
    }
}

#[cfg(test)]
fn notify_stream_termination_key(key: StreamKey, termination: ScreenCaptureStreamTermination) {
    if let Some(callback) = take_stream_termination_callback_key(key) {
        callback(termination);
    }
}

fn stream_termination_from_error(
    error_code: SCStreamErrorCode,
    description: SharedString,
) -> ScreenCaptureStreamTermination {
    if error_code == SCStreamErrorCode::UserStopped {
        ScreenCaptureStreamTermination::Cancelled
    } else {
        ScreenCaptureStreamTermination::Failed(description)
    }
}

fn stream_termination_from_nserror(error: Option<&NSError>) -> ScreenCaptureStreamTermination {
    match error {
        None => ScreenCaptureStreamTermination::Ended,
        Some(error) => stream_termination_from_error(
            SCStreamErrorCode(error.code()),
            error.localizedDescription().to_string().into(),
        ),
    }
}

struct StreamDelegateIvars;

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and `StreamDelegate`
    // does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "GPUIStreamDelegate"]
    #[ivars = StreamDelegateIvars]
    struct StreamDelegate;

    unsafe impl NSObjectProtocol for StreamDelegate {}

    unsafe impl SCStreamDelegate for StreamDelegate {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop_with_error(&self, stream: &SCStream, error: Option<&NSError>) {
            notify_stream_termination(stream, stream_termination_from_nserror(error));
        }
    }
);

impl StreamDelegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(StreamDelegateIvars);
        // SAFETY: `NSObject`'s `init` is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

struct StreamOutputIvars {
    frame_callback: Mutex<Option<FrameCallback>>,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and `StreamOutput`
    // does not implement `Drop`. Ivars drop the frame callback when the
    // Objective-C object is deallocated.
    #[unsafe(super(NSObject))]
    #[name = "GPUIStreamOutput"]
    #[ivars = StreamOutputIvars]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output_sample_buffer_of_type(
            &self,
            _stream: &SCStream,
            sample_buffer: &Objc2CMSampleBuffer,
            of_type: SCStreamOutputType,
        ) {
            if of_type != SCStreamOutputType::Screen {
                return;
            }

            let sample_buffer =
                unsafe { CMSampleBuffer::wrap_under_get_rule(ptr::from_ref(sample_buffer).cast()) };
            let Some(buffer) = sample_buffer.image_buffer() else {
                return;
            };

            let guard = self
                .ivars()
                .frame_callback
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            // `FrameCallback` is only required to be `Send`, not `Sync`. Keep
            // the mutex held while invoking it so callbacks are serialized
            // without widening that bound or changing their ownership model.
            if let Some(callback) = guard.as_ref() {
                callback(ScreenCaptureFrame(buffer));
            }
        }
    }
);

impl StreamOutput {
    fn new(frame_callback: FrameCallback) -> Retained<Self> {
        let this = Self::alloc().set_ivars(StreamOutputIvars {
            frame_callback: Mutex::new(Some(frame_callback)),
        });
        // SAFETY: `NSObject`'s `init` is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

impl ScreenCaptureSource for MacScreenCaptureSource {
    fn metadata(&self) -> Result<SourceMetadata> {
        // SAFETY: Marked unsafe conservatively by objc2.
        let display_id = unsafe { self.sc_display.displayID() };
        let size = unsafe {
            let display_mode_ref = CGDisplayCopyDisplayMode(display_id);
            let width = CGDisplayModeGetPixelWidth(display_mode_ref);
            let height = CGDisplayModeGetPixelHeight(display_mode_ref);
            CGDisplayModeRelease(display_mode_ref);
            size(DevicePixels(width as i32), DevicePixels(height as i32))
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
        let (mut tx, rx) = oneshot::channel();
        let meta = self.metadata().unwrap();

        let excluded_windows = NSArray::<SCWindow>::from_retained_slice(&[]);
        // SAFETY: `initWithDisplay:excludingWindows:` retains the display and
        // copies the window list it needs.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &self.sc_display,
                &excluded_windows,
            )
        };
        let configuration = unsafe { SCStreamConfiguration::new() };
        unsafe {
            configuration.setScalesToFit(true);
            configuration.setPixelFormat(PIXEL_FORMAT_BGRA);
            configuration.setWidth(meta.resolution.width.0 as usize);
            configuration.setHeight(meta.resolution.height.0 as usize);
        }

        let delegate = StreamDelegate::new();
        let output = StreamOutput::new(frame_callback);
        // SAFETY: filter, configuration, and delegate remain alive for the
        // initializer; `SCStream` retains the filter and configuration.
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &configuration,
                Some(ProtocolObject::from_ref(&*delegate)),
            )
        };

        if let Err(error) = unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Screen,
                None,
            )
        } {
            tx.send(Err(anyhow!(
                "failed to add stream output {}",
                error.localizedDescription()
            )))
            .ok();
            return rx;
        }

        register_stream_termination_callback(&stream, termination_callback);

        let tx = Rc::new(RefCell::new(Some(tx)));
        // `RcBlock` requires `Fn`. Take the owned start state on the first
        // (and only) completion-handler invocation, matching `get_sources`.
        let pending_start = Rc::new(RefCell::new(Some((stream.clone(), output, delegate, meta))));
        let handler = RcBlock::new(move |error: *mut NSError| {
            let result = if let Some(error) = unsafe { error.as_ref() } {
                if let Some((stream, _, _, _)) = pending_start.borrow_mut().take() {
                    take_stream_termination_callback(&stream);
                }
                Err(anyhow!(
                    "failed to start screen capture stream {}",
                    error.localizedDescription()
                ))
            } else {
                match pending_start.borrow_mut().take() {
                    Some((sc_stream, sc_stream_output, sc_stream_delegate, meta)) => {
                        Ok(Box::new(MacScreenCaptureStream {
                            meta,
                            sc_stream,
                            sc_stream_output,
                            _sc_stream_delegate: sc_stream_delegate,
                        }) as Box<dyn ScreenCaptureStream>)
                    }
                    None => Err(anyhow!(
                        "screen capture start handler invoked more than once"
                    )),
                }
            };
            if let Some(tx) = tx.borrow_mut().take() {
                tx.send(result).ok();
            }
        });
        unsafe {
            stream.startCaptureWithCompletionHandler(Some(&handler));
        }
        rx
    }
}

impl ScreenCaptureStream for MacScreenCaptureStream {
    fn metadata(&self) -> Result<SourceMetadata> {
        Ok(self.meta.clone())
    }
}

impl Drop for MacScreenCaptureStream {
    fn drop(&mut self) {
        notify_stream_termination(&self.sc_stream, ScreenCaptureStreamTermination::Cancelled);

        if let Err(error) = unsafe {
            self.sc_stream.removeStreamOutput_type_error(
                ProtocolObject::from_ref(&*self.sc_stream_output),
                SCStreamOutputType::Screen,
            )
        } {
            log::error!(
                "failed to remove stream output {}",
                error.localizedDescription()
            );
        }

        let handler = RcBlock::new(|error: *mut NSError| {
            if let Some(error) = unsafe { error.as_ref() } {
                log::error!(
                    "failed to stop screen capture stream {}",
                    error.localizedDescription()
                );
            }
        });
        unsafe {
            self.sc_stream
                .stopCaptureWithCompletionHandler(Some(&handler));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    struct CallbackDropSignal(mpsc::Sender<()>);

    impl Drop for CallbackDropSignal {
        fn drop(&mut self) {
            self.0.send(()).unwrap();
        }
    }

    #[test]
    fn user_stopped_stream_error_is_cancelled() {
        assert_eq!(
            stream_termination_from_error(
                SCStreamErrorCode::UserStopped,
                "The stream was stopped by the user".into()
            ),
            ScreenCaptureStreamTermination::Cancelled
        );
    }

    #[test]
    fn runtime_stream_error_preserves_its_failure_description() {
        assert_eq!(
            stream_termination_from_error(
                SCStreamErrorCode::InternalError,
                "Screen capture failed".into()
            ),
            ScreenCaptureStreamTermination::Failed("Screen capture failed".into())
        );
    }

    #[test]
    fn nil_stream_error_is_ended() {
        assert_eq!(
            stream_termination_from_nserror(None),
            ScreenCaptureStreamTermination::Ended
        );
    }

    #[test]
    fn terminal_notification_is_delivered_once() {
        let key = 0x1;
        let (sender, receiver) = mpsc::channel();
        register_stream_termination_callback_key(
            key,
            Box::new(move |termination| sender.send(termination).unwrap()),
        );

        notify_stream_termination_key(
            key,
            ScreenCaptureStreamTermination::Failed("capture device disconnected".into()),
        );
        notify_stream_termination_key(key, ScreenCaptureStreamTermination::Ended);

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

        drop(StreamOutput::new(callback));

        assert_eq!(receiver.recv_timeout(Duration::from_millis(100)), Ok(()));
    }

    #[test]
    fn stream_output_clone_keeps_frame_callback_alive_until_last_owner_drops() {
        let (sender, receiver) = mpsc::channel();
        let drop_signal = CallbackDropSignal(sender);
        let callback: FrameCallback = Box::new(move |_| {
            let _ = &drop_signal;
        });

        let output = StreamOutput::new(callback);
        let output_clone = output.clone();
        drop(output);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        drop(output_clone);
        assert_eq!(receiver.recv_timeout(Duration::from_millis(100)), Ok(()));
    }
}
