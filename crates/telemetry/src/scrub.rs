//! Removes file paths, file names, user names and volume names from outgoing Sentry data.
//!
//! Stack frames and debug images keep their code paths (needed for symbolication) with the
//! user and volume segments anonymized. Every other string has paths replaced by `<path>`.
//! If an event can't be scrubbed, it is dropped rather than sent.

use std::sync::LazyLock;

use regex::Regex;
use sentry::protocol::{Breadcrumb, Event};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const REDACTED_PATH: &str = "<path>";

static USER_SEGMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/Users/[^/\n]+").expect("valid regex"));
static VOLUME_SEGMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/Volumes/[^/\n]+").expect("valid regex"));
// File names often contain spaces, so a path runs until a newline, a quote, or a `: ` separator.
static PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(^|[\s(\[{<='",])((?:file://)?~?/(?:[^\n"'`:]|:\S)*)"#).expect("valid regex")
});

pub(crate) fn event(event: Event<'static>) -> Option<Event<'static>> {
    let mut event = scrub_value(event)?;
    event.server_name = None;
    event.user = None;
    event.request = None;
    Some(event)
}

pub(crate) fn breadcrumb(breadcrumb: Breadcrumb) -> Option<Breadcrumb> {
    scrub_value(breadcrumb)
}

fn scrub_value<T: Serialize + DeserializeOwned>(value: T) -> Option<T> {
    let mut json = serde_json::to_value(value).ok()?;
    walk(&mut json, false);
    serde_json::from_value(json).ok()
}

fn walk(value: &mut Value, in_code_location: bool) {
    match value {
        Value::String(text) => {
            *text = if in_code_location {
                anonymize(text)
            } else {
                redact(text)
            };
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| walk(item, in_code_location)),
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let code_location = in_code_location || key == "frames" || key == "debug_meta";
                walk(child, code_location);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn anonymize(text: &str) -> String {
    let text = USER_SEGMENT.replace_all(text, "/Users/<user>");
    VOLUME_SEGMENT
        .replace_all(&text, "/Volumes/<volume>")
        .into_owned()
}

fn redact(text: &str) -> String {
    PATH.replace_all(text, |caps: &regex::Captures<'_>| {
        format!("{}{REDACTED_PATH}", &caps[1])
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentry::protocol::{Exception, Frame, Stacktrace};

    #[test]
    fn redacts_paths_with_spaces_in_messages() {
        let text = "failed to read /Users/bob/Documents/Tax Return 2025.pdf: permission denied";
        assert_eq!(redact(text), "failed to read <path>: permission denied");
    }

    #[test]
    fn redacts_home_relative_and_file_urls() {
        assert_eq!(redact("open ~/Desktop/secret.txt"), "open <path>");
        assert_eq!(redact("url=file:///Volumes/Work/a.key"), "url=<path>");
        assert_eq!(redact("(at /private/var/x)"), "(at <path>");
    }

    #[test]
    fn keeps_text_without_paths() {
        let text = "called `Result::unwrap()` on an `Err` value: NotFound";
        assert_eq!(redact(text), text);
        assert_eq!(redact("3/4 folders scanned"), "3/4 folders scanned");
    }

    #[test]
    fn anonymizes_code_locations_without_removing_them() {
        assert_eq!(
            anonymize("/Users/bob/.cargo/git/checkouts/zed/crates/gpui/src/app.rs"),
            "/Users/<user>/.cargo/git/checkouts/zed/crates/gpui/src/app.rs"
        );
        assert_eq!(
            anonymize("/Volumes/Backup Disk/App.app"),
            "/Volumes/<volume>/App.app"
        );
    }

    #[test]
    fn scrubs_a_full_event() {
        let mut event = Event {
            message: Some("cannot trash /Users/bob/Projects/Acme Merger/plan.key".into()),
            server_name: Some("Bobs-MacBook-Pro.local".into()),
            ..Default::default()
        };
        event.exception.values.push(Exception {
            ty: "panic".into(),
            value: Some("unexpected entry at /Users/bob/Photos Library.photoslibrary".into()),
            stacktrace: Some(Stacktrace {
                frames: vec![Frame {
                    function: Some("scanner::walk".into()),
                    abs_path: Some("/Users/dev/src/scanner/src/walk.rs".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        });

        let scrubbed = super::event(event).expect("event is scrubbable");
        let json = serde_json::to_string(&scrubbed).unwrap();

        assert!(!json.contains("bob"), "{json}");
        assert!(!json.contains("Acme"), "{json}");
        assert!(!json.contains("Bobs-MacBook-Pro"), "{json}");
        assert_eq!(scrubbed.message.as_deref(), Some("cannot trash <path>"));
        let frame = &scrubbed.exception.values[0]
            .stacktrace
            .as_ref()
            .unwrap()
            .frames[0];
        assert_eq!(frame.function.as_deref(), Some("scanner::walk"));
        assert_eq!(
            frame.abs_path.as_deref(),
            Some("/Users/<user>/src/scanner/src/walk.rs")
        );
    }

    #[test]
    fn scrubs_breadcrumb_data() {
        let mut crumb = Breadcrumb {
            message: Some("scanning /Users/bob/Music".into()),
            ..Default::default()
        };
        crumb
            .data
            .insert("folder".into(), "/Users/bob/Secret".into());

        let scrubbed = breadcrumb(crumb).unwrap();

        assert_eq!(scrubbed.message.as_deref(), Some("scanning <path>"));
        assert_eq!(scrubbed.data["folder"], Value::from("<path>"));
    }
}
