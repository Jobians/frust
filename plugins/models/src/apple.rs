//! Backup exclusion for model files on Apple platforms.
//!
//! Models are large and re-downloadable, so they must not count against the user's device
//! backup. Failures are logged and ignored: a model that stays in the backup still works.

use std::path::Path;

/// Marks `path` as excluded from device backups. Best effort.
#[cfg(target_vendor = "apple")]
pub(crate) fn exclude_from_backup(path: &Path) {
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSNumber, NSString, NSURL, NSURLIsExcludedFromBackupKey};

    let Some(text) = path.to_str() else {
        log::warn!(
            "backup exclusion skipped for non-UTF-8 path {}",
            path.display()
        );
        return;
    };
    let url = NSURL::fileURLWithPath(&NSString::from_str(text));
    let number = NSNumber::numberWithBool(true);
    let value: &AnyObject = &number;
    // SAFETY: `NSURLIsExcludedFromBackupKey` expects an `NSNumber` boolean, which is what
    // `value` is.
    let result =
        unsafe { url.setResourceValue_forKey_error(Some(value), NSURLIsExcludedFromBackupKey) };
    if let Err(err) = result {
        log::warn!("could not exclude {} from backup: {err}", path.display());
    }
}

/// No-op off Apple platforms.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn exclude_from_backup(_path: &Path) {}
