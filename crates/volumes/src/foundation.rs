use std::path::Path;

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_foundation::{
    NSNumber, NSString, NSURL, NSURLResourceKey, NSURLVolumeAvailableCapacityForImportantUsageKey,
    NSURLVolumeLocalizedNameKey,
};

/// Free space including what macOS would purge for an important write
/// (`NSURLVolumeAvailableCapacityForImportantUsageKey`).
pub(crate) fn available_capacity_for_important_usage(path: &Path) -> Option<u64> {
    autoreleasepool(|_| {
        // SAFETY: the key is a Foundation constant.
        let value = resource_value(path, unsafe {
            NSURLVolumeAvailableCapacityForImportantUsageKey
        })?;
        let number = value.downcast::<NSNumber>().ok()?;
        u64::try_from(number.longLongValue()).ok()
    })
}

/// The volume's name as Finder shows it, for example "Macintosh HD".
pub(crate) fn volume_localized_name(path: &Path) -> Option<String> {
    autoreleasepool(|_| {
        // SAFETY: the key is a Foundation constant.
        let value = resource_value(path, unsafe { NSURLVolumeLocalizedNameKey })?;
        let name = value.downcast::<NSString>().ok()?;
        Some(name.to_string())
    })
}

fn resource_value(path: &Path, key: &NSURLResourceKey) -> Option<Retained<AnyObject>> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?));
    let mut value: Option<Retained<AnyObject>> = None;
    // SAFETY: `key` is a valid resource key, and `value` receives an owned object.
    unsafe { url.getResourceValue_forKey_error(&mut value, key) }.ok()?;
    value
}
