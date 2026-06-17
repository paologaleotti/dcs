//! Pure export planner (§6.9, decision #36). Takes the resolved dialog settings
//! (`ExportRequest`) plus the in-scope photos (`ExportItem`s the conductor builds
//! from the current selection/filter) and decides *everything*: which file of
//! each photo to copy, where it lands, how name collisions resolve, and the
//! dry-run sentence. No disk access — it only decides. `dcs-io` executes the
//! resulting `ExportPlan` verbatim and makes no choices of its own, so the
//! dialog's live preview and the real run are the same artifact (§6.1).
//!
//! Copy-only in v1; never overwrites (§6.6). Tag-keyed scope, the `{tag}` token,
//! and multi-tag flatten/duplicate land with the Tags slice — exclusive time
//! groups need none of that here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::photo::{Photo, PhotoId};

/// Which files of each photo to copy (§6.3). `Jpeg`/`Raw` copy only that role
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

/// Destination folder layout (§6.4). `GroupAsFolders` reuses the active grouping
/// (one folder per group title), the export-side payoff of the grouping model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Together,
    SplitJpegRaw,
    MirrorSource,
    GroupAsFolders,
}

/// Name-collision policy (§6.6). Never overwrite: `Skip` drops the colliding
/// file, `Rename` appends `-1`, `-2`, … before the extension. Default `Rename`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collision {
    Skip,
    Rename,
}

/// The role a file plays — drives the `SplitJpegRaw` layout and the per-role
/// counts in the dry-run sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRole {
    Jpeg,
    Raw,
}

/// An opt-in rename template (§6.6): a token string over `{name}`, `{date}`,
/// `{time}`, `{group}`, `{seq}`. Off by default (originals keep their names).
/// The extension always comes from the source file, never the template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameTemplate(pub String);

/// The resolved §6 stages as a plain request. The conductor builds this from the
/// dialog; the planner consumes it. Scope lives in the `ExportItem` list, not
/// here — the app resolves selection/filter into the in-scope photos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportRequest {
    pub dest: PathBuf,
    pub files: FileSelection,
    pub layout: Layout,
    pub collision: Collision,
    pub template: Option<NameTemplate>,
}

/// One in-scope photo handed to the planner, with the context the layout and
/// template need. `group_title` is the photo's current group (for
/// `GroupAsFolders` and `{group}`); `None` when ungrouped.
#[derive(Debug, Clone, Copy)]
pub struct ExportItem<'a> {
    pub photo: &'a Photo,
    pub group_title: Option<&'a str>,
}

/// One decided copy operation: a concrete source → dest with its role. The
/// executor copies these verbatim; it never computes a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOp {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub role: FileRole,
}

/// Why a photo contributed no file under the chosen selection (§6.3) — surfaced
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

/// The fully-decided plan (§6.9): the ordered ops, the skip report, the
/// collision count, and the dry-run sentence. Everything the dialog shows and
/// the executor runs comes from this one artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPlan {
    pub ops: Vec<ExportOp>,
    pub skipped: Vec<SkippedPhoto>,
    pub jpeg_count: usize,
    pub raw_count: usize,
    /// Ops whose dest had to change (rename policy) or that were dropped (skip
    /// policy) because the name was already taken — the "projected collisions".
    pub collisions: usize,
    pub dest: PathBuf,
    /// The one-sentence dry-run restatement (§6.7), e.g.
    /// `Copy 91 files (47 JPEG + 44 RAW) into "…", split JPEG/RAW, rename on collision.`
    pub summary: String,
}

/// Why a plan could not be produced. Domain-owned (§9); no I/O concepts.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExportError {
    /// No photos in scope (§13 step 1).
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
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    let mut collisions = 0usize;

    for (seq, item) in items.iter().enumerate() {
        let roles = selected_roles(item.photo, request.files, &mut skipped);
        for (role, source) in roles {
            let stem = file_stem(item, request, role, seq);
            let ext = extension(source);
            let folder = destination_folder(&request.dest, request.layout, role, item, source_root);
            match place(&folder, &stem, &ext, request.collision, &mut claimed) {
                Some(dest) => {
                    if dest_was_renamed(&dest, &folder, &stem, &ext) {
                        collisions += 1;
                    }
                    ops.push(ExportOp {
                        source: source.to_path_buf(),
                        dest,
                        role,
                    });
                }
                // Skip policy hit a taken name: nothing copied, counted as a collision.
                None => collisions += 1,
            }
        }
    }

    if ops.is_empty() {
        return Err(ExportError::NothingToCopy);
    }

    let jpeg_count = ops.iter().filter(|o| o.role == FileRole::Jpeg).count();
    let raw_count = ops.iter().filter(|o| o.role == FileRole::Raw).count();
    let summary = summarize(ops.len(), jpeg_count, raw_count, request);

    Ok(ExportPlan {
        ops,
        skipped,
        jpeg_count,
        raw_count,
        collisions,
        dest: request.dest.clone(),
        summary,
    })
}

const TOKENS: [&str; 5] = ["name", "date", "time", "group", "seq"];

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
    claimed: &mut HashSet<PathBuf>,
) -> Option<PathBuf> {
    let first = folder.join(join_name(stem, ext));
    if !claimed.contains(&first) {
        claimed.insert(first.clone());
        return Some(first);
    }
    match collision {
        Collision::Skip => None,
        Collision::Rename => {
            // Cascade `-1`, `-2`, … until a free name is found.
            for n in 1.. {
                let candidate = folder.join(join_name(&format!("{stem}-{n}"), ext));
                if !claimed.contains(&candidate) {
                    claimed.insert(candidate.clone());
                    return Some(candidate);
                }
            }
            unreachable!("the rename cascade always finds a free name")
        }
    }
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
/// layouts must sanitize (also satisfies the cross-platform rule, spec §1 #5).
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

/// The dry-run sentence (§6.7): verb, count, role breakdown, destination, layout,
/// and collision policy — the same text the button and preview show.
fn summarize(total: usize, jpeg: usize, raw: usize, request: &ExportRequest) -> String {
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
    format!(
        "Copy {total} {files} ({jpeg} JPEG + {raw} RAW) into \"{dest}\", {layout}, {collision}."
    )
}
