//! The presentation arms for every target with no native UI of a kind this
//! plugin drives: the alert arm off Android/iOS/macOS (desktop Linux,
//! Windows, web) and the toast arm off Android/iOS (macOS included). Every
//! request resolves [`PresentError::Unsupported`] on its first poll, and
//! nothing is ever live to dismiss.

#[cfg(not(target_os = "macos"))]
use super::{AlertHost, AlertOutcome, AlertSpec, Sender};
use super::{PresentError, ToastSpec, toast::ToastHost};

/// The refusing alert host.
#[cfg(not(target_os = "macos"))]
pub(crate) struct Host;

#[cfg(not(target_os = "macos"))]
impl AlertHost for Host {
    fn show_alert(
        _spec: AlertSpec,
        _tx: Sender<AlertOutcome>,
        _generation: u64,
    ) -> Result<(), PresentError> {
        Err(PresentError::Unsupported)
    }

    fn dismiss(_generation: u64) {}
}

/// The refusing toast host: no native toast arm on this target.
pub(crate) struct Toast;

impl ToastHost for Toast {
    fn show_toast(_spec: ToastSpec, _generation: u64) -> Result<(), PresentError> {
        Err(PresentError::Unsupported)
    }

    fn dismiss(_generation: u64) {}
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    use super::super::{ToastOutcome, show_toast, test_support};
    use super::*;

    /// Driven through the host directly rather than `show_alert`, so this
    /// test never touches the process-wide Busy slot `super::super`'s own
    /// serialized tests own.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn every_show_is_refused_unsupported() {
        use super::super::{ActionRole, oneshot};
        // A generation nothing is ever live under (see `oneshot`'s tests).
        let (tx, _rx) = oneshot::channel(u64::MAX);
        let spec = AlertSpec::new("t", "m").with_action("ok", "OK", ActionRole::Default);
        assert_eq!(
            Host::show_alert(spec, tx, u64::MAX),
            Err(PresentError::Unsupported)
        );
        Host::dismiss(u64::MAX);
    }

    #[test]
    fn every_toast_is_refused_unsupported() {
        assert_eq!(
            Toast::show_toast(ToastSpec::new("Saved"), u64::MAX),
            Err(PresentError::Unsupported)
        );
        Toast::dismiss(u64::MAX);

        // Through the public entry point: refused on the first poll, no handle.
        let _guard = test_support::serialize();
        let mut toast = show_toast(ToastSpec::new("Saved"));
        assert_eq!(toast.handle(), None);
        let poll: Poll<Result<ToastOutcome, PresentError>> =
            Pin::new(&mut toast).poll(&mut Context::from_waker(Waker::noop()));
        assert_eq!(poll, Poll::Ready(Err(PresentError::Unsupported)));
    }
}
