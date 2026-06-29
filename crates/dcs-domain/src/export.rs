//! Pure export planner. Takes the resolved dialog settings
//! (`ExportRequest`) plus the in-scope photos (`ExportItem`s the conductor builds
//! from the current selection/filter) and decides *everything*: which file of
//! each photo to copy, where it lands, how name collisions resolve, and the
//! dry-run sentence. No disk access — it only decides. `dcs-io` executes the
//! resulting `ExportPlan` verbatim and makes no choices of its own, so the
//! dialog's live preview and the real run are the same artifact.
//!
//! Copy-only in v1; never overwrites. The `{tag}` token resolves to each
//! photo's primary tag (first by band order); multi-tag flatten/duplicate
//! placement stays deferred (v1.1).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::crops::CropEdit;
use crate::photo::{Orientation, Photo, PhotoId};

/// Which files of each photo to copy. `Jpeg`/`Raw` copy only that role
/// and skip photos lacking it. `Both` copies the JPEG **and** the RAW, skipping
/// any photo that doesn't have both. `Any` copies whatever files the photo has,
/// never skipping (the as-shot superset) — the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSelection {
    Jpeg,
    Raw,
    Both,
    Any,
}

/// Destination folder layout. `GroupAsFolders` reuses the active grouping
/// (one folder per group title), the export-side payoff of the grouping model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Together,
    SplitJpegRaw,
    MirrorSource,
    GroupAsFolders,
}

/// Name-collision policy. Never overwrite: `Skip` drops the colliding
/// file, `Rename` appends `-1`, `-2`, … before the extension. Default `Rename`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collision {
    Skip,
    Rename,
}

/// The role a file plays — drives the `SplitJpegRaw` layout and the per-role
/// counts in the dry-run sentence. `Sidecar` rides alongside its photo's primary
/// file (same folder), carried only when the request opts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    Jpeg,
    Raw,
    Sidecar,
}

/// An opt-in rename template: a token string over `{name}`, `{date}`,
/// `{time}`, `{group}`, `{seq}`, `{tag}`. Off by default (originals keep their
/// names). The extension always comes from the source file, never the template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameTemplate(pub String);

/// The resolved export stages as a plain request. The conductor builds this from the
/// dialog; the planner consumes it. Scope lives in the `ExportItem` list, not
/// here — the app resolves selection/filter into the in-scope photos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRequest {
    pub dest: PathBuf,
    pub files: FileSelection,
    pub layout: Layout,
    pub collision: Collision,
    pub template: Option<NameTemplate>,
    /// Carry each photo's adjacent sidecars (e.g. XMP) alongside its files.
    pub sidecars: bool,
    /// When a photo is cropped, the export output is the cropped render. With
    /// this on, the untouched original JPEG is *also* copied, into an
    /// `originals/` subfolder under the destination root. Off by default.
    pub include_uncropped_originals: bool,
}

/// One in-scope photo handed to the planner, with the context the layout and
/// template need. `group_title` is the photo's current group (for
/// `GroupAsFolders` and `{group}`); `None` when ungrouped. `primary_tag` is the
/// photo's lowest-id (earliest-created) tag, for the `{tag}` token; `None` when
/// untagged.
#[derive(Debug, Clone, Copy)]
pub struct ExportItem<'a> {
    pub photo: &'a Photo,
    pub group_title: Option<&'a str>,
    pub primary_tag: Option<&'a str>,
    /// Adjacent sidecar files (e.g. XMP) the conductor found next to this photo's
    /// files. Copied only when `ExportRequest::sidecars` is set; empty otherwise.
    pub sidecars: &'a [PathBuf],
    /// The photo's committed crop, if any. When set, its JPEG op is a
    /// `RenderCrop` rather than a plain copy.
    pub crop: Option<CropEdit>,
}

/// How the executor materializes one op. `Copy` is the byte-for-byte atomic
/// copy (the v1 default for everything). `RenderCrop` decodes the source,
/// applies the straighten+crop, and re-encodes to the dest — the only op that
/// touches pixels. The planner still decided the source, dest, and that this op
/// renders; the executor makes no choice of its own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OpKind {
    Copy,
    /// Decode, apply EXIF orientation then the crop, re-encode. Orientation
    /// rides along because re-encoding drops the source's EXIF tag, so the
    /// output must be upright in pixels.
    RenderCrop {
        edit: CropEdit,
        orientation: Orientation,
    },
}

/// One decided operation: a concrete source → dest with its role and kind. The
/// executor runs these verbatim; it never computes a path or decides a kind.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportOp {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub role: FileRole,
    pub kind: OpKind,
}

/// Why a photo contributed no file under the chosen selection — surfaced
/// in the dialog's "(show)" affordance, selectable back into the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `Raw`-only selection, photo has no RAW.
    NoRaw,
    /// `Jpeg`-only selection, photo has no JPEG.
    NoJpeg,
}

/// A photo excluded from the plan because the file selection matched none of its
/// files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippedPhoto {
    pub id: PhotoId,
    pub reason: SkipReason,
}

/// The fully-decided plan: the ordered ops, the skip report, the
/// collision count, and the dry-run sentence. Everything the dialog shows and
/// the executor runs comes from this one artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportPlan {
    pub ops: Vec<ExportOp>,
    pub skipped: Vec<SkippedPhoto>,
    pub jpeg_count: usize,
    pub raw_count: usize,
    pub sidecar_count: usize,
    /// Untouched-original copies emitted into `originals/` when
    /// `include_uncropped_originals` is on. Counted apart from `jpeg_count` so a
    /// cropped photo's render and its original aren't conflated in the summary.
    pub original_count: usize,
    /// Ops whose dest had to change (rename policy) or that were dropped (skip
    /// policy) because the name was already taken — the "projected collisions".
    pub collisions: usize,
    pub dest: PathBuf,
    /// The one-sentence dry-run restatement, e.g.
    /// `Copy 91 files (47 JPEG + 44 RAW) into "…", split JPEG/RAW, rename on collision.`
    pub summary: String,
}

/// Why a plan could not be produced. Domain-owned; no I/O concepts.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExportError {
    /// No photos in scope.
    #[error("nothing in scope to export")]
    EmptyScope,
    /// Scope was non-empty but the file selection matched no files at all.
    #[error("no files match the selection (every photo skipped)")]
    NothingToCopy,
    /// The rename template referenced an unknown token.
    #[error("unknown template token `{{{0}}}`")]
    BadTemplate(String),
}

/// Plan an export: decide every copy operation, collision rename, and skip from
/// the in-scope `items` and the `request`. Pure — no disk access. `source_root`
/// is the scanned folder, used to recreate the subtree under `MirrorSource`.
///
/// Ops come out in `items` order, JPEG before RAW within a photo. Collisions are
/// resolved against the set of already-claimed dest paths, so two photos with
/// the same basename in one folder cascade `-1`, `-2`, … (or skip).
pub fn plan_export(
    items: &[ExportItem],
    source_root: &Path,
    request: &ExportRequest,
) -> Result<ExportPlan, ExportError> {
    if items.is_empty() {
        return Err(ExportError::EmptyScope);
    }
    if let Some(template) = &request.template {
        validate_template(&template.0)?;
    }

    let mut ops: Vec<ExportOp> = Vec::new();
    let mut skipped: Vec<SkippedPhoto> = Vec::new();
    let mut claimed: HashSet<String> = HashSet::new();
    let mut collisions = 0usize;
    // Counted inline rather than by post-filtering `ops` on role: the
    // `include_uncropped_originals` copies carry `FileRole::Jpeg` but must not
    // inflate `jpeg_count` (that would double-count every cropped photo).
    let mut jpeg_count = 0usize;
    let mut raw_count = 0usize;
    let mut sidecar_count = 0usize;
    let mut original_count = 0usize;
    let mut crop_count = 0usize;

    for (seq, item) in items.iter().enumerate() {
        let roles = selected_roles(item.photo, request.files, &mut skipped);
        // The folder of the photo's first copied file; sidecars ride into it.
        let mut primary_folder: Option<PathBuf> = None;
        for (role, source) in roles {
            let stem = file_stem(item, request, role, seq);
            let ext = extension(source);
            let folder = destination_folder(&request.dest, request.layout, role, item, source_root);
            if primary_folder.is_none() {
                primary_folder = Some(folder.clone());
            }
            // Only the JPEG carries the crop; the RAW (when present) copies as-is.
            let kind = match (role, item.crop) {
                (FileRole::Jpeg, Some(edit)) => OpKind::RenderCrop {
                    edit,
                    orientation: item.photo.orientation,
                },
                _ => OpKind::Copy,
            };
            match place(&folder, &stem, &ext, request.collision, &mut claimed) {
                Some(dest) => {
                    if dest_was_renamed(&dest, &folder, &stem, &ext) {
                        collisions += 1;
                    }
                    match role {
                        FileRole::Jpeg => jpeg_count += 1,
                        FileRole::Raw => raw_count += 1,
                        FileRole::Sidecar => {}
                    }
                    if matches!(kind, OpKind::RenderCrop { .. }) {
                        crop_count += 1;
                    }
                    ops.push(ExportOp {
                        source: source.to_path_buf(),
                        dest,
                        role,
                        kind,
                    });
                }
                // Skip policy hit a taken name: nothing copied, counted as a collision.
                None => collisions += 1,
            }
            // With the opt-in, a cropped JPEG also drops its untouched original
            // into an `originals/` subfolder under the destination root — a plain
            // copy, run through the same collision machinery.
            if request.include_uncropped_originals && matches!(kind, OpKind::RenderCrop { .. }) {
                let folder = request.dest.join("originals");
                match place(&folder, &stem, &ext, request.collision, &mut claimed) {
                    Some(dest) => {
                        if dest_was_renamed(&dest, &folder, &stem, &ext) {
                            collisions += 1;
                        }
                        original_count += 1;
                        ops.push(ExportOp {
                            source: source.to_path_buf(),
                            dest,
                            role: FileRole::Jpeg,
                            kind: OpKind::Copy,
                        });
                    }
                    None => collisions += 1,
                }
            }
        }
        // Sidecars only ride along when the photo actually contributed a file, so
        // a skipped photo never leaves an orphan XMP behind.
        if request.sidecars
            && let Some(folder) = &primary_folder
        {
            for source in item.sidecars {
                // Match the photo's stem (template-renamed or original) so the
                // sidecar↔file link survives the copy; the extension stays the
                // sidecar's own.
                let stem = sidecar_stem(item, request, source, seq);
                let ext = extension(source);
                match place(folder, &stem, &ext, request.collision, &mut claimed) {
                    Some(dest) => {
                        if dest_was_renamed(&dest, folder, &stem, &ext) {
                            collisions += 1;
                        }
                        sidecar_count += 1;
                        ops.push(ExportOp {
                            source: source.clone(),
                            dest,
                            role: FileRole::Sidecar,
                            kind: OpKind::Copy,
                        });
                    }
                    None => collisions += 1,
                }
            }
        }
    }

    if ops.is_empty() {
        return Err(ExportError::NothingToCopy);
    }

    let summary = summarize(
        ops.len(),
        jpeg_count,
        raw_count,
        sidecar_count,
        original_count,
        crop_count,
        request,
    );

    Ok(ExportPlan {
        ops,
        skipped,
        jpeg_count,
        raw_count,
        sidecar_count,
        original_count,
        collisions,
        dest: request.dest.clone(),
        summary,
    })
}

const TOKENS: [&str; 6] = ["name", "date", "time", "group", "seq", "tag"];

/// The files a photo contributes under the selection, in JPEG-then-RAW order.
/// Records a skip when a single-role selection finds no matching file.
fn selected_roles<'a>(
    photo: &'a Photo,
    selection: FileSelection,
    skipped: &mut Vec<SkippedPhoto>,
) -> Vec<(FileRole, &'a Path)> {
    let jpeg = photo.files.jpeg.as_deref();
    let raw = photo.files.raw.as_deref();
    let mut out = Vec::new();
    match selection {
        FileSelection::Jpeg => match jpeg {
            Some(p) => out.push((FileRole::Jpeg, p)),
            None => skipped.push(SkippedPhoto {
                id: photo.id,
                reason: SkipReason::NoJpeg,
            }),
        },
        FileSelection::Raw => match raw {
            Some(p) => out.push((FileRole::Raw, p)),
            None => skipped.push(SkippedPhoto {
                id: photo.id,
                reason: SkipReason::NoRaw,
            }),
        },
        // Both demands the pair: a photo missing either file is skipped, with the
        // missing role as the reason (so the "(show)" report points at it).
        FileSelection::Both => match (jpeg, raw) {
            (Some(j), Some(r)) => {
                out.push((FileRole::Jpeg, j));
                out.push((FileRole::Raw, r));
            }
            (None, _) => skipped.push(SkippedPhoto {
                id: photo.id,
                reason: SkipReason::NoJpeg,
            }),
            (_, None) => skipped.push(SkippedPhoto {
                id: photo.id,
                reason: SkipReason::NoRaw,
            }),
        },
        // Any is the as-shot superset: whatever exists, never skipped.
        FileSelection::Any => {
            if let Some(p) = jpeg {
                out.push((FileRole::Jpeg, p));
            }
            if let Some(p) = raw {
                out.push((FileRole::Raw, p));
            }
        }
    }
    out
}

/// The destination folder for one file, before the filename is appended.
fn destination_folder(
    dest: &Path,
    layout: Layout,
    role: FileRole,
    item: &ExportItem,
    source_root: &Path,
) -> PathBuf {
    match layout {
        Layout::Together => dest.to_path_buf(),
        Layout::SplitJpegRaw => match role {
            FileRole::Jpeg => dest.join("JPEG"),
            FileRole::Raw => dest.join("RAW"),
            FileRole::Sidecar => {
                unreachable!("sidecars ride into the primary folder, never destination_folder")
            }
        },
        Layout::GroupAsFolders => dest.join(sanitize(item.group_title.unwrap_or("Ungrouped"))),
        Layout::MirrorSource => {
            let source = role_source(item.photo, role);
            match source
                .and_then(|s| s.parent())
                .and_then(|p| p.strip_prefix(source_root).ok())
            {
                Some(rel) => dest.join(rel),
                None => dest.to_path_buf(),
            }
        }
    }
}

/// Decide the final dest path inside `folder`, honoring the collision policy.
/// Returns the claimed path, or `None` when `Skip` policy drops a taken name.
fn place(
    folder: &Path,
    stem: &str,
    ext: &str,
    collision: Collision,
    claimed: &mut HashSet<String>,
) -> Option<PathBuf> {
    let first = folder.join(join_name(stem, ext));
    // `insert` returns false when the normalized key is already taken — that is a
    // collision even if the byte-exact path differs only in case.
    if claimed.insert(collision_key(&first)) {
        return Some(first);
    }
    match collision {
        Collision::Skip => None,
        Collision::Rename => {
            // Cascade `-1`, `-2`, … until a free name is found.
            for n in 1.. {
                let candidate = folder.join(join_name(&format!("{stem}-{n}"), ext));
                if claimed.insert(collision_key(&candidate)) {
                    return Some(candidate);
                }
            }
            unreachable!("the rename cascade always finds a free name")
        }
    }
}

/// Normalized key for collision detection. Filenames on the default Windows
/// (NTFS) and macOS (APFS) filesystems are case-insensitive, so two ops whose
/// dest paths differ only in case (`a.JPG` vs `a.jpg`) would otherwise both be
/// emitted and the second would silently overwrite the first when the dumb
/// executor copies them — breaking "never overwrite". Case-folding the whole
/// path makes the planner treat them as a collision on every platform.
///
/// This folds *folder* case too, not just the filename — and that direction is
/// load-bearing: under `GroupAsFolders`, two group titles differing only in case
/// (`Temple` vs `temple`) resolve to the same real directory on a
/// case-insensitive FS, so files inside them must collide. Folding only the
/// filename would let those overwrite on the very platforms dcs primarily
/// targets. The cost is the inverse, on a case-sensitive FS: two genuinely
/// distinct case-different folders may see an avoidable rename. That is the
/// deliberate, safe-by-default tradeoff — it can never overwrite. Unicode
/// normalization (NFC vs NFD, which APFS also folds) is a known remaining gap.
fn collision_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// The filename stem for an op: the template expansion when one is set, else the
/// source file's own stem.
fn file_stem(item: &ExportItem, request: &ExportRequest, role: FileRole, seq: usize) -> String {
    match &request.template {
        Some(template) => expand_template(&template.0, item, seq),
        None => role_source(item.photo, role)
            .and_then(|s| s.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

fn role_source(photo: &Photo, role: FileRole) -> Option<&Path> {
    match role {
        FileRole::Jpeg => photo.files.jpeg.as_deref(),
        FileRole::Raw => photo.files.raw.as_deref(),
        FileRole::Sidecar => {
            unreachable!("sidecars carry their own source path, never resolved by role")
        }
    }
}

/// The stem for a sidecar op: the template expansion (so the sidecar follows its
/// renamed photo) when a template is set, else the sidecar file's own stem.
fn sidecar_stem(item: &ExportItem, request: &ExportRequest, source: &Path, seq: usize) -> String {
    match &request.template {
        Some(template) => expand_template(&template.0, item, seq),
        None => source
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

fn extension(source: &Path) -> String {
    source
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn join_name(stem: &str, ext: &str) -> String {
    if ext.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{ext}")
    }
}

/// True when `dest`'s filename differs from the un-renamed `stem.ext` — i.e. the
/// collision cascade changed it.
fn dest_was_renamed(dest: &Path, folder: &Path, stem: &str, ext: &str) -> bool {
    dest != folder.join(join_name(stem, ext))
}

/// Reject a template referencing an unknown `{token}` before any planning.
fn validate_template(template: &str) -> Result<(), ExportError> {
    for token in tokens(template) {
        if !TOKENS.contains(&token.as_str()) {
            return Err(ExportError::BadTemplate(token));
        }
    }
    Ok(())
}

/// Expand a validated template for one item. `{date}`/`{time}` come from the
/// capture time (`nodate`/`000000` when undated); `{seq}` is 1-based, padded.
fn expand_template(template: &str, item: &ExportItem, seq: usize) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            // A stray '{' with no '}' is literal text (validate_template passed).
            out.push_str(&rest[open..]);
            return sanitize(&out);
        };
        let token = &rest[open + 1..open + close];
        out.push_str(&expand_token(token, item, seq));
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    sanitize(&out)
}

fn expand_token(token: &str, item: &ExportItem, seq: usize) -> String {
    match token {
        "name" => role_name_stem(item.photo),
        "group" => item.group_title.unwrap_or("Ungrouped").to_string(),
        "tag" => item.primary_tag.unwrap_or("untagged").to_string(),
        "seq" => format!("{:04}", seq + 1),
        "date" => match item.photo.captured_at {
            Some(dt) => format!("{:04}{:02}{:02}", dt.year(), u8::from(dt.month()), dt.day()),
            None => "nodate".to_string(),
        },
        "time" => match item.photo.captured_at {
            Some(dt) => format!("{:02}{:02}{:02}", dt.hour(), dt.minute(), dt.second()),
            None => "000000".to_string(),
        },
        other => format!("{{{other}}}"),
    }
}

fn role_name_stem(photo: &Photo) -> String {
    photo
        .display_path()
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The `{token}` names found in a template, in order.
fn tokens(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        out.push(rest[open + 1..open + close].to_string());
        rest = &rest[open + close + 1..];
    }
    out
}

/// Make a string safe as a single path component on every platform: replace the
/// separators and Windows-reserved characters with `-`, collapse to a fallback
/// when the result is empty. Group titles carry `/` (dates) and `·`, so folder
/// layouts must sanitize (also satisfies the cross-platform rule).
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The dry-run sentence: verb, count, role breakdown, destination, layout,
/// and collision policy — the same text the button and preview show.
fn summarize(
    total: usize,
    jpeg: usize,
    raw: usize,
    sidecar: usize,
    originals: usize,
    cropped: usize,
    request: &ExportRequest,
) -> String {
    let dest = request.dest.display();
    let layout = match request.layout {
        Layout::Together => "one folder",
        Layout::SplitJpegRaw => "split JPEG/RAW",
        Layout::MirrorSource => "mirroring the source tree",
        Layout::GroupAsFolders => "a folder per group",
    };
    let collision = match request.collision {
        Collision::Skip => "skip on collision",
        Collision::Rename => "rename on collision",
    };
    let files = if total == 1 { "file" } else { "files" };
    let mut breakdown = format!("{jpeg} JPEG + {raw} RAW");
    if sidecar > 0 {
        breakdown.push_str(&format!(" + {sidecar} sidecar"));
    }
    if originals > 0 {
        breakdown.push_str(&format!(" + {originals} original"));
    }
    // A render-and-crop run isn't a pure copy; say so when any op crops.
    let verb = if cropped > 0 { "Export" } else { "Copy" };
    let mut summary =
        format!("{verb} {total} {files} ({breakdown}) into \"{dest}\", {layout}, {collision}");
    if cropped > 0 {
        summary.push_str(&format!(" ({cropped} cropped)"));
    }
    summary.push('.');
    summary
}
