use crate::{BiometricKind, BiometricStatus};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAPolicy};

pub fn biometric_status() -> BiometricStatus {
    let context = unsafe { LAContext::new() };
    let available = unsafe {
        context
            .canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthenticationWithBiometrics)
            .is_ok()
    };
    if available {
        BiometricStatus::Available(BiometricKind::TouchId)
    } else {
        BiometricStatus::Unavailable
    }
}

pub fn authenticate_biometric(reason: &str, callback: Box<dyn FnOnce(bool) + Send>) {
    let context = unsafe { LAContext::new() };
    let reason = NSString::from_str(reason);
    let callback = std::sync::Mutex::new(Some(callback));

    // Keep the context alive until the reply runs. `evaluatePolicy` may run on a
    // private queue, so the retain is transferred as a raw pointer (Send).
    let context_ptr = Retained::into_raw(context);
    let handler = RcBlock::new(move |success: Bool, _error: *mut NSError| {
        let _context = unsafe { Retained::from_raw(context_ptr) };
        if let Some(cb) = callback.lock().ok().and_then(|mut guard| guard.take()) {
            cb(success.as_bool());
        }
    });

    unsafe {
        (*context_ptr).evaluatePolicy_localizedReason_reply(
            LAPolicy::DeviceOwnerAuthenticationWithBiometrics,
            &reason,
            &handler,
        );
    }
}
