// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::{self, NonNull};

use accessibility::{AXError, AXUIElement, Error};
use accessibility_sys::{AXObserver, pid_t};
use objc2_core_foundation::{CFRetained, CFRunLoop, CFString, kCFRunLoopCommonModes};

/// An observer for accessibility events.
pub struct Observer {
    callback: *mut (),
    dtor: unsafe fn(*mut ()),
    observer: ManuallyDrop<CFRetained<AXObserver>>,
}

static_assertions::assert_not_impl_any!(Observer: Send);

/// Helper type for building an [`Observer`].
//
// This type exists to carry type information about our callback `F` to the call
// to `new` from the call to `install`. It exists because of the following
// constraints:
//
// * Creating the observer object can fail, e.g. if the app in question is no
//   longer running.
// * The `Observer` often needs to go inside an object that is also referenced
//   by the callback. This necessitates the use of APIs like
//   [`std::rc::Rc::make_cyclic`], which unfortunately is not fallible.
// * `Observer` should not know about the type of its callback, both because
//   that type usually cannot be named and for convenience.
// * We want to avoid double indirection on calls to the callback, which
//   necessitates knowing the type of `F` when creating the system observer
//   object during the call to `new`.
//
// This means we make creation of the Observer a two-step process. `new` can
// fail and can be called before the call to `make_cyclic`. `install` is
// infallible and can be called inside, meaning the callback passed to it can
// capture a weak pointer to our object.
pub struct ObserverBuilder<F>(CFRetained<AXObserver>, PhantomData<F>);

impl Observer {
    /// Creates a new observer for an app, given its `pid`.
    ///
    /// Note that you must call [`ObserverBuilder::install`] on the result of
    /// this function and supply a callback for the observer to have any effect.
    pub fn new<F: Fn(CFRetained<AXUIElement>, &str) + 'static>(
        pid: pid_t,
    ) -> Result<ObserverBuilder<F>, Error> {
        // SAFETY: We just create an observer here, and check the return code.
        // The callback cannot be called yet. The API guarantees that F will be
        // supplied as the callback in the call to install (and the 'static
        // bound on F means we don't need to worry about variance).
        let mut observer: *mut AXObserver = ptr::null_mut();
        let err = unsafe {
            AXObserver::create(
                pid,
                Some(internal_callback::<F>),
                NonNull::new_unchecked(&mut observer),
            )
        };
        make_result(err)?;
        // SAFETY: AXObserverCreate succeeded, so `observer` is a valid +1 object.
        let observer = unsafe { CFRetained::from_raw(NonNull::new_unchecked(observer)) };
        Ok(ObserverBuilder(observer, PhantomData))
    }
}

impl<F: Fn(CFRetained<AXUIElement>, &str) + 'static> ObserverBuilder<F> {
    /// Installs the observer with the supplied callback into the current
    /// thread's run loop.
    pub fn install(self, callback: F) -> Observer {
        // SAFETY: We know from typestate that the observer will call
        // internal_callback::<F>. F is 'static, so even if our destructor is
        // not run it will remain valid to call.
        unsafe {
            let source = self.0.run_loop_source();
            CFRunLoop::current().unwrap().add_source(Some(&source), kCFRunLoopCommonModes);
        }
        Observer {
            callback: Box::into_raw(Box::new(callback)) as *mut (),
            dtor: destruct::<F>,
            observer: ManuallyDrop::new(self.0),
        }
    }
}

unsafe fn destruct<T>(ptr: *mut ()) {
    let _ = unsafe { Box::from_raw(ptr as *mut T) };
}

impl Drop for Observer {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.observer);
            (self.dtor)(self.callback);
        }
    }
}

impl Observer {
    pub fn add_notification(
        &self,
        elem: &AXUIElement,
        notification: &'static str,
    ) -> Result<(), Error> {
        make_result(unsafe {
            self.observer.add_notification(
                elem.as_sys(),
                &CFString::from_static_str(notification),
                self.callback as *mut c_void,
            )
        })
    }

    pub fn remove_notification(
        &self,
        elem: &AXUIElement,
        notification: &'static str,
    ) -> Result<(), Error> {
        make_result(unsafe {
            self.observer
                .remove_notification(elem.as_sys(), &CFString::from_static_str(notification))
        })
    }
}

unsafe extern "C-unwind" fn internal_callback<F: Fn(CFRetained<AXUIElement>, &str) + 'static>(
    _observer: NonNull<AXObserver>,
    elem: NonNull<accessibility_sys::AXUIElement>,
    notif: NonNull<CFString>,
    data: *mut c_void,
) {
    let callback = unsafe { &*(data as *const F) };
    // SAFETY: `elem` is a valid, unretained (+0) reference for the duration of
    // this call, and shares layout with `accessibility::AXUIElement`.
    let elem = unsafe { CFRetained::retain(elem) };
    let elem = unsafe { CFRetained::cast_unchecked::<AXUIElement>(elem) };
    // SAFETY: `notif` is a valid reference for the duration of this call.
    let notif = unsafe { notif.as_ref() };
    callback(elem, &notif.to_string());
}

fn make_result(err: accessibility_sys::AXError) -> Result<(), Error> {
    match AXError::from_raw(err) {
        Some(err) => Err(Error::Ax(err)),
        None => Ok(()),
    }
}
