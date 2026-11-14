//! Files dragged in from the desktop (Explorer, Finder, a file manager).
//!
//! While the operating system drags files over a window it sends no mouse
//! moves (on Windows the OLE drag loop owns the mouse), so egui's pointer
//! position is stale, or gone because the pointer left the window to pick
//! the file up. Matching the drop to whatever was under the pointer then
//! silently found nothing. Instead, each frame [`begin_frame`] asks the OS
//! where the cursor is (falling back to egui's pointer where we can't), and
//! [`super::browser::drop_on`] hands the files to the view under that
//! position exactly like an item dragged from the Browser. Files no view
//! claims are placed on a new track by [`end_frame`], so a drop never does
//! nothing.

use std::path::PathBuf;

use eframe::egui::{self, Pos2, Rect};

use crate::app::{TesseraApp, View};
use crate::browser::{BrowserItem, DropTarget};

#[derive(Clone, Default)]
struct State {
    /// Files being dragged over this window right now.
    hovering: bool,
    /// Files dropped this frame and not yet claimed by a view.
    dropped: Vec<PathBuf>,
    /// Where the cursor is, in this window's points.
    pos: Option<Pos2>,
}

fn key(ctx: &egui::Context) -> egui::Id {
    egui::Id::new(("os_file_drop", ctx.viewport_id()))
}

fn clear(ctx: &egui::Context) {
    let key = key(ctx);
    ctx.data_mut(|d| d.insert_temp(key, State::default()));
}

fn state(ctx: &egui::Context) -> State {
    let key = key(ctx);
    ctx.data(|d| d.get_temp::<State>(key)).unwrap_or_default()
}

/// Reads this window's file hover and drop. Call before drawing its views.
pub fn begin_frame(ctx: &egui::Context) {
    let (hovering, dropped) = ctx.input(|i| {
        (
            !i.raw.hovered_files.is_empty(),
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect::<Vec<_>>(),
        )
    });
    let pos = if hovering || !dropped.is_empty() {
        cursor_pos(ctx)
    } else {
        None
    };
    // The OS sends nothing while the cursor moves during a drag, so keep
    // redrawing to move the highlight along with it.
    if hovering {
        ctx.request_repaint();
    }
    let key = key(ctx);
    ctx.data_mut(|d| d.insert_temp(key, State { hovering, dropped, pos }));
}

/// The OS cursor position in this window's points.
fn cursor_pos(ctx: &egui::Context) -> Option<Pos2> {
    os_cursor_pos(ctx).or_else(|| ctx.input(|i| i.pointer.latest_pos()))
}

#[cfg(windows)]
fn os_cursor_pos(ctx: &egui::Context) -> Option<Pos2> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let inner = ctx.input(|i| i.viewport().inner_rect)?;
    let mut p = POINT::default();
    // SAFETY: GetCursorPos only writes the POINT it is given.
    unsafe { GetCursorPos(&mut p) }.ok()?;
    // Physical screen pixels (winit makes the process per-monitor DPI aware)
    // to points relative to the window's client area.
    let ppp = ctx.pixels_per_point();
    Some(Pos2::new(p.x as f32 / ppp, p.y as f32 / ppp) - inner.min.to_vec2())
}

#[cfg(not(windows))]
fn os_cursor_pos(_ctx: &egui::Context) -> Option<Pos2> {
    None
}

/// While files hover over `rect` (and the visible part of `ui`), returns
/// `Some(None)` so the caller can highlight it; when they are dropped there,
/// claims them and returns `Some(Some(item))`.
pub fn drop_on(ui: &egui::Ui, rect: Rect) -> Option<Option<BrowserItem>> {
    let ctx = ui.ctx();
    let s = state(ctx);
    if !s.hovering && s.dropped.is_empty() {
        return None;
    }
    let pos = s.pos?;
    if !rect.contains(pos) || !ui.clip_rect().contains(pos) {
        return None;
    }
    if s.dropped.is_empty() {
        return Some(None);
    }
    clear(ctx);
    Some(Some(item_for(s.dropped)))
}

fn item_for(mut paths: Vec<PathBuf>) -> BrowserItem {
    if paths.len() == 1 {
        BrowserItem::Sample(paths.remove(0))
    } else {
        BrowserItem::Files(paths)
    }
}

/// Places files no view claimed (dropped over a panel, or where the cursor
/// position is unknown) on a new track in `view`. Call after drawing.
pub fn end_frame(app: &mut TesseraApp, ctx: &egui::Context, view: View) {
    let s = state(ctx);
    if s.dropped.is_empty() {
        return;
    }
    clear(ctx);
    app.label_edit("Import Files");
    let target = new_track_target(app, view);
    app.drop_item(&item_for(s.dropped), target);
}

/// Where audio dropped on empty space in `view` goes: a new track, with the
/// clip at the playhead in the Arrangement.
pub fn new_track_target(app: &TesseraApp, view: View) -> DropTarget {
    match view {
        View::Session => DropTarget::NewTrack,
        View::Arrangement => DropTarget::NewArrangementTrack(app.song_beat().max(0.0)),
    }
}
