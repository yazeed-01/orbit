//! Builds the radial menu for a selection.

use std::path::Path;

use crate::actions::{Action, Doc, Img, Media};
use crate::explorer::Ctx;
use crate::util;

#[derive(Clone)]
pub struct Item {
    pub label: String,
    pub icon: char,
    pub color: u32,
    pub hint: String,
    pub kind: Kind,
}

#[derive(Clone)]
pub enum Kind {
    Run(Action),
    Sub(String, Vec<Item>),
}

fn act(label: &str, icon: char, color: u32, a: Action) -> Item {
    Item { label: label.into(), icon, color, hint: String::new(), kind: Kind::Run(a) }
}

fn sub(label: &str, icon: char, color: u32, items: Vec<Item>) -> Item {
    Item { label: label.into(), icon, color, hint: String::new(), kind: Kind::Sub(label.into(), items) }
}

impl Item {
    fn hint(mut self, h: &str) -> Self {
        self.hint = h.into();
        self
    }
}

// palette
const VIOLET: u32 = 0x7C5CFF;
const BLUE: u32 = 0x3B82F6;
const CYAN: u32 = 0x06B6D4;
const GREEN: u32 = 0x22C55E;
const AMBER: u32 = 0xF59E0B;
const ORANGE: u32 = 0xF97316;
const PINK: u32 = 0xEC4899;
const RED: u32 = 0xEF4444;
const SLATE: u32 = 0x94A3B8;
const TEAL: u32 = 0x14B8A6;

// Segoe Fluent / MDL2 glyphs
pub mod ic {
    pub const SHARE: char = '\u{E72D}';
    pub const ARCHIVE: char = '\u{E7B8}';
    pub const CONVERT: char = '\u{E8AB}';
    pub const RESIZE: char = '\u{E740}';
    pub const COMPRESS: char = '\u{E945}';
    pub const ROTATE: char = '\u{E7AD}';
    pub const MORE: char = '\u{E712}';
    pub const COPY: char = '\u{E8C8}';
    pub const HASH: char = '\u{E928}';
    pub const INFO: char = '\u{E946}';
    pub const RENAME: char = '\u{E8AC}';
    pub const TERMINAL: char = '\u{E756}';
    pub const CODE: char = '\u{E943}';
    pub const SIZE: char = '\u{EDA2}';
    pub const SORT: char = '\u{E8CB}';
    pub const NEW_FOLDER: char = '\u{E8F4}';
    pub const NEW_FILE: char = '\u{E8A5}';
    pub const ADD: char = '\u{E710}';
    pub const PASTE: char = '\u{E77F}';
    pub const RECEIVE: char = '\u{E896}';
    pub const EYE: char = '\u{E7B3}';
    pub const PICTURE: char = '\u{E8B9}';
    pub const VIDEO: char = '\u{E714}';
    pub const AUDIO: char = '\u{E8D6}';
    pub const CUT: char = '\u{E8C6}';
    pub const CAMERA: char = '\u{E722}';
    pub const MUTE: char = '\u{E74F}';
    pub const VOLUME: char = '\u{E767}';
    pub const PDF: char = '\u{EA90}';
    pub const DOC: char = '\u{E8A5}';
    pub const SPLIT: char = '\u{E8C6}';
    pub const MERGE: char = '\u{E71B}';
    pub const EXTRACT: char = '\u{E8B7}';
    pub const STAR: char = '\u{E734}';
    pub const CLEAN: char = '\u{E75C}';
    pub const GIF: char = '\u{E7F4}';
    pub const TABLE: char = '\u{E80A}';
    pub const SLIDES: char = '\u{E8A1}';
    pub const FLATTEN: char = '\u{E8A9}';
    pub const DELETE: char = '\u{E74D}';
    pub const MOVE: char = '\u{E8DE}';
    pub const EDIT: char = '\u{E70F}';
    pub const SPEED: char = '\u{E916}';
    pub const GRID: char = '\u{E80A}';
    pub const FOLDER: char = '\u{E8B7}';
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FileKind {
    Image,
    Video,
    Audio,
    Pdf,
    Word,
    Slides,
    Sheet,
    Archive,
    Text,
    Other,
}

pub fn kind_of(p: &Path) -> FileKind {
    match util::ext(p).as_str() {
        "png" | "jpg" | "jpeg" | "jfif" | "webp" | "gif" | "bmp" | "ico" | "tif" | "tiff" | "avif" | "heic" | "heif" | "tga" | "psd" => FileKind::Image,
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "3gp" | "ts" | "mts" | "m2ts" | "ogv" => FileKind::Video,
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "oga" | "opus" | "wma" | "aiff" | "aif" | "alac" | "amr" | "ac3" => FileKind::Audio,
        "pdf" => FileKind::Pdf,
        "doc" | "docx" | "odt" | "rtf" => FileKind::Word,
        "ppt" | "pptx" | "odp" | "pps" | "ppsx" => FileKind::Slides,
        "xls" | "xlsx" | "ods" | "csv" | "xlsm" => FileKind::Sheet,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "tbz2" | "txz" | "cab" | "iso" => FileKind::Archive,
        "txt" | "md" | "json" | "js" | "mjs" | "tsx" | "jsx" | "py" | "java" | "c" | "cpp" | "h" | "hpp" | "cs" | "go" | "rs" | "html" | "htm"
        | "css" | "scss" | "xml" | "yml" | "yaml" | "toml" | "ini" | "cfg" | "log" | "sh" | "bat" | "cmd" | "ps1" | "sql" | "php" | "rb" | "kt"
        | "swift" | "dart" | "vue" | "svelte" | "lua" | "env" | "svg" => FileKind::Text,
        _ => FileKind::Other,
    }
}

fn archive_menu() -> Item {
    sub(
        "Archive",
        ic::ARCHIVE,
        AMBER,
        vec![
            act("RAR", ic::ARCHIVE, AMBER, Action::Archive("rar")).hint("Needs WinRAR, else ZIP"),
            act("ZIP", ic::ARCHIVE, ORANGE, Action::Archive("zip")),
            act("7z", ic::ARCHIVE, PINK, Action::Archive("7z")).hint("Best compression"),
        ],
    )
}

fn share() -> Item {
    act("Share", ic::SHARE, GREEN, Action::Share).hint("QR code · same Wi-Fi")
}

fn dest_menu(label: &str, icon: char, color: u32, mv: bool) -> Item {
    let a = |p: Option<std::path::PathBuf>| if mv { Action::MoveTo(p) } else { Action::CopyTo(p) };
    let mut items = vec![act("Choose…", ic::FOLDER, color, a(None)).hint("Pick any folder")];
    for p in crate::config::recent_folders().into_iter().take(9) {
        let hint = p.display().to_string();
        items.push(act(&util::name(&p), ic::FOLDER, color, a(Some(p))).hint(&hint));
    }
    sub(label, icon, color, items)
}

fn common_more(extra: Vec<Item>) -> Item {
    let mut items = vec![dest_menu("Move to", ic::MOVE, ORANGE, true), dest_menu("Copy to", ic::COPY, BLUE, false)];
    items.extend(extra);
    items.extend([
        act("Cut", ic::CUT, ORANGE, Action::Cut).hint("Ctrl+X"),
        act("Copy path", ic::COPY, SLATE, Action::CopyPath),
        act("Copy hash", ic::HASH, TEAL, Action::Hash).hint("SHA-256 + MD5"),
        act("Batch rename", ic::RENAME, BLUE, Action::BatchRename),
        act("Properties", ic::INFO, SLATE, Action::Properties),
    ]);
    sub("More", ic::MORE, SLATE, items)
}

fn image_menu(multi: bool) -> Vec<Item> {
    let mut extra = vec![act("Image info", ic::INFO, CYAN, Action::ImageInfo).hint("Size, camera, GPS")];
    if multi {
        let c = |l: &str, k: &'static str, h: &str| act(l, ic::GRID, PINK, Action::Img(Img::Combine(k))).hint(h);
        extra.push(sub("Combine", ic::GRID, PINK, vec![c("Grid", "grid", "Collage in a grid"), c("Side by side", "h", "One row"), c("Stacked", "v", "One column")]));
    }
    let fmt = |l: &str, e: &'static str, c: u32| act(l, ic::PICTURE, c, Action::Img(Img::Convert(e)));
    let mut convert = vec![
        fmt("PNG", "png", BLUE),
        fmt("JPG", "jpg", ORANGE),
        fmt("WEBP", "webp", GREEN),
        fmt("GIF", "gif", PINK),
        fmt("BMP", "bmp", SLATE),
        fmt("ICO", "ico", AMBER),
        fmt("AVIF", "avif", VIOLET),
        fmt("TIFF", "tiff", TEAL),
        fmt("JPEG", "jpeg", ORANGE),
    ];
    convert.push(act("PDF", ic::PDF, RED, Action::ImagesToPdf).hint(if multi { "All images → one PDF" } else { "Image → PDF" }));
    let rs = |l: &str, s: Img, h: &str| act(l, ic::RESIZE, CYAN, Action::Img(s)).hint(h);
    let resize = vec![
        rs("720p", Img::Height(720), "1280×720"),
        rs("1080p", Img::Height(1080), "1920×1080"),
        rs("2K", Img::Height(1440), "2560×1440"),
        rs("4K", Img::Height(2160), "3840×2160"),
        rs("50%", Img::Percent(50), "Half size"),
        rs("25%", Img::Percent(25), "Quarter size"),
        rs("Width…", Img::Width, "Set width, keep ratio"),
        rs("Custom", Img::Custom, "Set exact X × Y"),
    ];
    let rot = |l: &str, f: &'static str, sfx: &'static str| act(l, ic::ROTATE, VIOLET, Action::Img(Img::Rotate(f, sfx)));
    let rotate = vec![
        rot("Right 90°", "transpose=1", "_rotated"),
        rot("Left 90°", "transpose=2", "_rotated"),
        rot("180°", "transpose=1,transpose=1", "_rotated"),
        rot("Flip H", "hflip", "_flipped"),
        rot("Flip V", "vflip", "_flipped"),
    ];
    vec![
        sub("Convert", ic::CONVERT, BLUE, convert),
        sub("Resize", ic::RESIZE, CYAN, resize),
        act("Compress", ic::COMPRESS, AMBER, Action::Img(Img::Compress)).hint("Smaller file, same look"),
        sub("Rotate", ic::ROTATE, VIOLET, rotate),
        share(),
        archive_menu(),
        common_more({
            extra.extend([
                act("Strip EXIF", ic::CLEAN, TEAL, Action::Img(Img::Strip)).hint("Remove metadata / GPS"),
                act("Favicon set", ic::STAR, AMBER, Action::Img(Img::Favicon)).hint("16 → 512 px + .ico"),
                act("Base64", ic::CODE, VIOLET, Action::CopyBase64).hint("Copy as data URI"),
            ]);
            extra
        }),
    ]
}

fn edit_media(video: bool) -> Item {
    let sp = |l: &str, p: u32| act(l, ic::SPEED, TEAL, Action::Media(Media::Speed(p)));
    let vol = |l: &str, p: u32, h: &str| act(l, ic::VOLUME, GREEN, Action::Media(Media::Volume(p))).hint(h);
    let mut items = vec![
        act("Trim", ic::CUT, VIOLET, Action::Media(Media::Trim)).hint("Cut start → end"),
        sub("Speed", ic::SPEED, TEAL, vec![sp("0.5×", 50), sp("0.75×", 75), sp("1.25×", 125), sp("1.5×", 150), sp("2×", 200)]),
        sub("Volume", ic::VOLUME, GREEN, vec![vol("50%", 50, "Quieter"), vol("150%", 150, "Louder"), vol("200%", 200, "Twice as loud"), vol("Custom", 0, "Any percent"), act("Normalize", ic::VOLUME, GREEN, Action::Media(Media::Normalize)).hint("Even out loudness")]),
        act("Merge", ic::MERGE, VIOLET, Action::Media(Media::Merge)).hint("Join selected clips in name order"),
    ];
    if video {
        let vr = |l: &str, h: u32| act(l, ic::RESIZE, CYAN, Action::Media(Media::VResize(h)));
        let rot = |l: &str, f: &'static str| act(l, ic::ROTATE, VIOLET, Action::Media(Media::VRotate(f)));
        items.extend([
            sub("Rotate", ic::ROTATE, VIOLET, vec![rot("Right 90°", "transpose=1"), rot("Left 90°", "transpose=2"), rot("180°", "transpose=1,transpose=1"), rot("Flip H", "hflip")]),
            sub("Resize", ic::RESIZE, CYAN, vec![vr("480p", 480), vr("720p", 720), vr("1080p", 1080), vr("2K", 1440), vr("4K", 2160)]),
            act("Mute", ic::MUTE, RED, Action::Media(Media::Mute)).hint("Remove audio track"),
            act("Grab frame", ic::CAMERA, TEAL, Action::Media(Media::Frame)).hint("Middle frame → PNG"),
        ]);
    }
    sub("Edit", ic::EDIT, VIOLET, items)
}

fn video_menu() -> Vec<Item> {
    let vc = |l: &str, e: &'static str, c: u32| act(l, ic::VIDEO, c, Action::Media(Media::VConvert(e)));
    let ac = |l: &str, e: &'static str, c: u32| act(l, ic::AUDIO, c, Action::Media(Media::ExtractAudio(e)));
    vec![
        sub(
            "Convert",
            ic::CONVERT,
            BLUE,
            vec![
                vc("MP4", "mp4", BLUE),
                vc("MKV", "mkv", VIOLET),
                vc("WEBM", "webm", GREEN),
                vc("AVI", "avi", ORANGE),
                vc("MOV", "mov", SLATE),
                act("GIF", ic::GIF, PINK, Action::Media(Media::ToGif)).hint("Smooth, palette-optimised"),
            ],
        ),
        sub(
            "To audio",
            ic::AUDIO,
            PINK,
            vec![ac("MP3", "mp3", PINK), ac("WAV", "wav", BLUE), ac("M4A", "m4a", VIOLET), ac("FLAC", "flac", GREEN), ac("OGG", "ogg", ORANGE)],
        ),
        act("Compress", ic::COMPRESS, AMBER, Action::Media(Media::VCompress)).hint("H.264, much smaller"),
        edit_media(true),
        share(),
        archive_menu(),
        common_more(vec![]),
    ]
}

fn audio_menu() -> Vec<Item> {
    let a = |l: &str, k: &'static str, c: u32, h: &str| act(l, ic::AUDIO, c, Action::Media(Media::AConvert(k))).hint(h);
    vec![
        sub(
            "Convert",
            ic::CONVERT,
            BLUE,
            vec![
                a("MP3", "mp3", PINK, "192 kbps"),
                a("MP3 128k", "mp3_128", PINK, "Smaller"),
                a("MP3 320k", "mp3_320", PINK, "Best MP3"),
                a("WAV", "wav", BLUE, "Uncompressed"),
                a("FLAC", "flac", GREEN, "Lossless"),
                a("M4A", "m4a", VIOLET, "AAC"),
                a("OGG", "ogg", ORANGE, "Vorbis"),
                a("OPUS", "opus", TEAL, "Tiny & great"),
                a("AAC", "aac", CYAN, "Raw AAC"),
            ],
        ),
        edit_media(false),
        share(),
        archive_menu(),
        common_more(vec![]),
    ]
}

fn pdf_menu(multi: bool) -> Vec<Item> {
    let rot = |l: &str, d: i64| act(l, ic::ROTATE, VIOLET, Action::PdfRotate(d));
    let mut pages = vec![
        rot("Right 90°", 90),
        rot("Left 90°", 270),
        rot("180°", 180),
        act("Split", ic::SPLIT, ORANGE, Action::PdfSplit).hint("One PDF per page"),
    ];
    if multi {
        pages.push(act("Merge", ic::MERGE, VIOLET, Action::PdfMerge).hint("Combine into one PDF"));
    }
    vec![
        act("To Word", ic::DOC, BLUE, Action::Doc(Doc::PdfToWord)).hint("Editable .docx"),
        act("To images", ic::PICTURE, CYAN, Action::PdfToImages).hint("PNG per page"),
        act("Compress", ic::COMPRESS, AMBER, Action::PdfCompress).hint("Shrink images inside"),
        sub(if multi { "Pages · Merge" } else { "Pages" }, ic::SPLIT, ORANGE, pages),
        share(),
        archive_menu(),
        common_more(vec![]),
    ]
}

fn word_menu() -> Vec<Item> {
    let w = |l: &str, e: &'static str, c: u32| act(l, ic::DOC, c, Action::Doc(Doc::Office(e)));
    vec![
        act("To PDF", ic::PDF, RED, Action::Doc(Doc::Office("pdf"))),
        sub("Convert", ic::CONVERT, BLUE, vec![w("DOCX", "docx", BLUE), w("TXT", "txt", SLATE), w("HTML", "html", ORANGE), w("ODT", "odt", GREEN), w("RTF", "rtf", VIOLET)]),
        share(),
        archive_menu(),
        common_more(vec![]),
    ]
}

fn slides_menu() -> Vec<Item> {
    vec![
        act("To PDF", ic::PDF, RED, Action::Doc(Doc::Office("pdf"))),
        act("To images", ic::PICTURE, BLUE, Action::Doc(Doc::SlidesImages)).hint("PNG per slide"),
        act("To PPTX", ic::SLIDES, ORANGE, Action::Doc(Doc::Office("pptx"))),
        share(),
        archive_menu(),
        common_more(vec![]),
    ]
}

fn sheet_menu() -> Vec<Item> {
    vec![
        act("To PDF", ic::PDF, RED, Action::Doc(Doc::Office("pdf"))),
        act("To CSV", ic::TABLE, GREEN, Action::Doc(Doc::Office("csv"))),
        act("To XLSX", ic::TABLE, GREEN, Action::Doc(Doc::Office("xlsx"))),
        share(),
        archive_menu(),
        common_more(vec![]),
    ]
}

fn archive_file_menu() -> Vec<Item> {
    vec![
        act("Extract here", ic::EXTRACT, AMBER, Action::Extract(false)),
        act("Extract to folder", ic::EXTRACT, ORANGE, Action::Extract(true)).hint("Into its own folder"),
        share(),
        common_more(vec![]),
    ]
}

fn text_menu(any_json: bool) -> Vec<Item> {
    let mut v = vec![act("Open in VS Code", ic::CODE, BLUE, Action::VsCode), act("To PDF", ic::PDF, RED, Action::Doc(Doc::Office("pdf")))];
    if any_json {
        v.push(act("Format JSON", ic::CODE, AMBER, Action::FormatJson).hint("Pretty-print in place"));
    }
    v.extend([
        share(),
        archive_menu(),
        common_more(vec![
            act("Count lines", ic::SORT, TEAL, Action::CountLines),
            act("Base64", ic::CODE, VIOLET, Action::CopyBase64).hint("Copy encoded"),
        ]),
    ]);
    v
}

fn organize_menu() -> Item {
    sub(
        "Organize",
        ic::SORT,
        TEAL,
        vec![
            act("By type", ic::SORT, TEAL, Action::OrganizeType).hint("Images, Videos, Docs…"),
            act("By date", ic::SORT, CYAN, Action::OrganizeDate).hint("Year-Month folders"),
            act("Flatten", ic::FLATTEN, VIOLET, Action::Flatten).hint("Pull files out of subfolders"),
            act("Extract all", ic::EXTRACT, AMBER, Action::ExtractAll).hint("Unpack every archive"),
        ],
    )
}

fn folder_menu() -> Vec<Item> {
    vec![
        share(),
        archive_menu(),
        act("Terminal", ic::TERMINAL, SLATE, Action::Terminal),
        act("VS Code", ic::CODE, BLUE, Action::VsCode),
        act("Folder size", ic::SIZE, CYAN, Action::FolderSize),
        organize_menu(),
        common_more(vec![]),
    ]
}

fn background_menu() -> Vec<Item> {
    let nf = |l: &str, e: &'static str, c: u32| act(l, ic::NEW_FILE, c, Action::NewFile(e));
    vec![
        act("New folder", ic::NEW_FOLDER, AMBER, Action::NewFolder),
        sub(
            "New file",
            ic::ADD,
            BLUE,
            vec![
                nf("Text", "txt", SLATE),
                nf("Markdown", "md", VIOLET),
                nf("Python", "py", BLUE),
                nf("JavaScript", "js", AMBER),
                nf("TypeScript", "ts", CYAN),
                nf("HTML", "html", ORANGE),
                nf("CSS", "css", BLUE),
                nf("JSON", "json", GREEN),
                nf("Batch", "bat", SLATE),
                nf("PowerShell", "ps1", BLUE),
                nf("Rust", "rs", ORANGE),
                nf("C#", "cs", VIOLET),
                nf("Java", "java", RED),
                nf(".gitignore", "gitignore", PINK),
                nf(".env", "env", TEAL),
                nf("YAML", "yml", RED),
            ],
        ),
        act("Terminal", ic::TERMINAL, SLATE, Action::Terminal).hint("Open here"),
        act("VS Code", ic::CODE, BLUE, Action::VsCode).hint("Open this folder"),
        act("Paste", ic::PASTE, VIOLET, Action::Paste).hint("Files, text or image · Ctrl+V"),
        act("Receive", ic::RECEIVE, GREEN, Action::Receive).hint("Phone → this folder"),
        sub(
            "Folder",
            ic::MORE,
            SLATE,
            vec![
                act("Folder size", ic::SIZE, CYAN, Action::FolderSize),
                organize_menu(),
                act("Hidden files", ic::EYE, AMBER, Action::ToggleHidden).hint("Show / hide"),
                act("Copy path", ic::COPY, SLATE, Action::CopyPath),
                act("Properties", ic::INFO, SLATE, Action::Properties),
            ],
        ),
    ]
}

/// (title, items) for this context.
pub fn build(ctx: &Ctx) -> (String, Vec<Item>) {
    if ctx.background {
        return (util::name(&ctx.folder), background_menu());
    }
    let items = &ctx.items;
    let n = items.len();
    let dirs = items.iter().filter(|p| p.is_dir()).count();
    let title = if n == 1 {
        util::name(&items[0])
    } else if dirs == n {
        format!("{n} folders")
    } else {
        let exts: std::collections::BTreeSet<String> = items.iter().map(|p| util::ext(p)).collect();
        if exts.len() == 1 && dirs == 0 {
            format!("{n} × {}", exts.iter().next().unwrap().to_uppercase())
        } else {
            format!("{n} items")
        }
    };
    if dirs == n {
        return (title, with_edit(folder_menu(), n));
    }
    if dirs > 0 {
        return (title, with_edit(vec![share(), archive_menu(), act("Copy path", ic::COPY, SLATE, Action::CopyPath), act("Cut", ic::CUT, ORANGE, Action::Cut), act("Properties", ic::INFO, SLATE, Action::Properties)], n));
    }
    let kinds: Vec<FileKind> = items.iter().map(|p| kind_of(p)).collect();
    let all = |k: FileKind| kinds.iter().all(|x| *x == k);
    let menu = if all(FileKind::Image) {
        image_menu(n > 1)
    } else if all(FileKind::Video) {
        video_menu()
    } else if all(FileKind::Audio) {
        audio_menu()
    } else if all(FileKind::Pdf) {
        pdf_menu(n > 1)
    } else if all(FileKind::Word) {
        word_menu()
    } else if all(FileKind::Slides) {
        slides_menu()
    } else if all(FileKind::Sheet) {
        sheet_menu()
    } else if all(FileKind::Archive) {
        archive_file_menu()
    } else if all(FileKind::Text) {
        text_menu(items.iter().any(|p| util::ext(p) == "json"))
    } else if kinds.iter().all(|k| matches!(k, FileKind::Video | FileKind::Audio)) {
        audio_menu()
    } else {
        vec![
            share(),
            archive_menu(),
            act("Copy path", ic::COPY, SLATE, Action::CopyPath),
            act("Copy hash", ic::HASH, TEAL, Action::Hash),
            act("Cut", ic::CUT, ORANGE, Action::Cut),
            act("Open in VS Code", ic::CODE, BLUE, Action::VsCode),
            act("Properties", ic::INFO, SLATE, Action::Properties),
        ]
    };
    (title, with_edit(menu, n))
}

/// Copy / Rename / Delete on every file & folder menu, placed just before "More".
fn with_edit(mut menu: Vec<Item>, n: usize) -> Vec<Item> {
    let edit = [
        act("Copy", ic::COPY, BLUE, Action::Copy).hint("Ctrl+C · paste anywhere"),
        act("Rename", ic::RENAME, VIOLET, Action::Rename).hint(if n > 1 { "Batch rename · F2" } else { "F2" }),
        act("Delete", ic::DELETE, RED, Action::Delete).hint("Recycle Bin · Shift = permanent"),
    ];
    let at = if menu.last().is_some_and(|i| i.label == "More") { menu.len() - 1 } else { menu.len() };
    menu.splice(at..at, edit);
    menu
}
