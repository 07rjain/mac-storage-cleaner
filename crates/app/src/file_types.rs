//! Rough file kinds from names, for coloring the treemap.

use std::ffi::OsStr;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileType {
    Video,
    Image,
    Audio,
    Document,
    Archive,
    App,
    Code,
    Developer,
    Other,
}

impl FileType {
    pub const ALL: [Self; 9] = [
        Self::Video,
        Self::Image,
        Self::Audio,
        Self::Document,
        Self::Archive,
        Self::App,
        Self::Code,
        Self::Developer,
        Self::Other,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Video => "Videos",
            Self::Image => "Images",
            Self::Audio => "Audio",
            Self::Document => "Documents",
            Self::Archive => "Archives and disk images",
            Self::App => "Apps and libraries",
            Self::Code => "Code",
            Self::Developer => "Build and developer data",
            Self::Other => "Other",
        }
    }

    /// The kind of a file, from its extension.
    pub fn of_file(name: &OsStr) -> Self {
        let Some(extension) = extension(name) else {
            return Self::Other;
        };
        let extension = extension.as_str();
        let table: [(&[&str], Self); 8] = [
            (VIDEO, Self::Video),
            (IMAGE, Self::Image),
            (AUDIO, Self::Audio),
            (DOCUMENT, Self::Document),
            (ARCHIVE, Self::Archive),
            (APP_FILES, Self::App),
            (CODE, Self::Code),
            (DEVELOPER, Self::Developer),
        ];
        table
            .into_iter()
            .find(|(extensions, _)| extensions.contains(&extension))
            .map_or(Self::Other, |(_, kind)| kind)
    }

    /// A folder macOS shows as one item, whose whole contents take its kind.
    pub fn of_bundle(name: &OsStr) -> Option<Self> {
        let extension = extension(name)?;
        let extension = extension.as_str();
        [
            (APP_BUNDLES, Self::App),
            (PHOTO_BUNDLES, Self::Image),
            (AUDIO_BUNDLES, Self::Audio),
            (VIDEO_BUNDLES, Self::Video),
            (DOCUMENT_BUNDLES, Self::Document),
            (DEVELOPER_BUNDLES, Self::Developer),
        ]
        .into_iter()
        .find(|(extensions, _)| extensions.contains(&extension))
        .map(|(_, kind)| kind)
    }
}

fn extension(name: &OsStr) -> Option<String> {
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
}

const VIDEO: &[&str] = &[
    "mp4", "mov", "m4v", "mkv", "avi", "wmv", "webm", "mpg", "mpeg", "3gp", "mts", "m2ts", "braw",
    "r3d", "prores",
];
const IMAGE: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "heic", "heif", "tif", "tiff", "bmp", "webp", "raw", "cr2", "cr3",
    "nef", "arw", "dng", "raf", "orf", "psd", "ai", "svg", "ico", "icns", "exr",
];
const AUDIO: &[&str] = &[
    "mp3", "m4a", "aac", "wav", "aif", "aiff", "flac", "alac", "ogg", "opus", "caf", "mid", "midi",
];
const DOCUMENT: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pages", "numbers", "key", "txt", "rtf",
    "md", "csv", "epub", "odt", "ods", "odp",
];
const ARCHIVE: &[&str] = &[
    "zip",
    "gz",
    "tgz",
    "bz2",
    "xz",
    "zst",
    "7z",
    "rar",
    "tar",
    "dmg",
    "iso",
    "img",
    "pkg",
    "mpkg",
    "xip",
    "sparseimage",
    "sparsebundle",
    "vmdk",
    "qcow2",
    "vdi",
];
const APP_FILES: &[&str] = &["dylib", "so", "a", "car", "nib", "ipa", "apk", "jar"];
const CODE: &[&str] = &[
    "rs", "c", "h", "cc", "cpp", "hpp", "m", "mm", "swift", "py", "js", "mjs", "ts", "tsx", "jsx",
    "go", "java", "kt", "rb", "php", "cs", "sh", "zsh", "html", "css", "scss", "json", "yaml",
    "yml", "toml", "xml", "sql", "lua", "dart", "vue", "svelte",
];
const DEVELOPER: &[&str] = &[
    "o",
    "rlib",
    "rmeta",
    "pcm",
    "pch",
    "swiftmodule",
    "swiftdoc",
    "class",
    "pyc",
    "wasm",
    "map",
    "idx",
    "pack",
    "db",
    "sqlite",
    "sqlite3",
    "log",
];
const APP_BUNDLES: &[&str] = &[
    "app",
    "framework",
    "appex",
    "bundle",
    "plugin",
    "kext",
    "prefpane",
    "xpc",
    "qlgenerator",
    "mdimporter",
];
const PHOTO_BUNDLES: &[&str] = &["photoslibrary", "photolibrary", "aplibrary"];
const AUDIO_BUNDLES: &[&str] = &["musiclibrary", "logicx", "band"];
const VIDEO_BUNDLES: &[&str] = &["fcpbundle", "imovielibrary", "tvlibrary"];
const DOCUMENT_BUNDLES: &[&str] = &["rtfd"];
const DEVELOPER_BUNDLES: &[&str] = &[
    "xcarchive",
    "xcodeproj",
    "xcworkspace",
    "xcassets",
    "dsym",
    "docset",
    "playground",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_classified_by_extension_ignoring_case() {
        let of = |name: &str| FileType::of_file(OsStr::new(name));
        assert_eq!(of("Holiday.MOV"), FileType::Video);
        assert_eq!(of("IMG_0001.HEIC"), FileType::Image);
        assert_eq!(of("song.flac"), FileType::Audio);
        assert_eq!(of("report.pdf"), FileType::Document);
        assert_eq!(of("Xcode_16.xip"), FileType::Archive);
        assert_eq!(of("libfoo.dylib"), FileType::App);
        assert_eq!(of("main.rs"), FileType::Code);
        assert_eq!(of("lib.rlib"), FileType::Developer);
        assert_eq!(of("README"), FileType::Other);
        assert_eq!(of(".zshrc"), FileType::Other);
    }

    #[test]
    fn bundles_give_their_contents_one_kind() {
        let of = |name: &str| FileType::of_bundle(OsStr::new(name));
        assert_eq!(of("Safari.app"), Some(FileType::App));
        assert_eq!(of("Photos Library.photoslibrary"), Some(FileType::Image));
        assert_eq!(of("App 2026-10-05.xcarchive"), Some(FileType::Developer));
        assert_eq!(of("Documents"), None);
        assert_eq!(of("notes.txt"), None);
    }
}
