//! Measure the compact title bar's close-button offsets, because the eye is
//! right and the box model is not obvious.
//!
//! Reported as the close button sitting too far left -- the gap to the right
//! edge visibly larger than the gap above and below it. The compact sheet in
//! `host_window.rs` sets `padding: 0 6px` on the header bar *and* `margin: 2px`
//! on the control buttons, so the right-hand gap is the sum of the two while
//! the vertical gap is only the margin. Arithmetic says 8px against 3px; this
//! prints what GTK actually allocates, which is the only version of that claim
//! worth putting in a commit.
//!
//! `CORDIAL_PROBE_SHEET=old` measures the sheet as it shipped, `new` the
//! candidate. Run both and compare -- a fix that nobody measured against the
//! unfixed control is how this project has been wrong before.
//!
//! ```
//! cargo run -p cordial-shell --example compact_bar_offsets
//! CORDIAL_PROBE_SHEET=old cargo run -p cordial-shell --example compact_bar_offsets
//! ```

use libadwaita as adw;
use libadwaita::glib;
use libadwaita::gtk;
use libadwaita::prelude::*;

const OLD: &str = ".probe headerbar { min-height: 30px; padding: 0 6px; } \
                   .probe headerbar windowcontrols button { \
                       min-width: 24px; min-height: 24px; padding: 0; margin: 2px; \
                   }";

const NEW: &str = ".probe headerbar { min-height: 30px; padding: 0 5px; } \
                   .probe headerbar windowcontrols button { \
                       min-width: 24px; min-height: 24px; padding: 0; margin: 2px; \
                   }";

/// The shipped sheet with one pixel taken off the close button's right margin.
/// Measured, across every variant tried here: the right gap comes out exactly
/// one larger than the vertical one, because the horizontal inset is a sum of
/// paddings while the vertical is whatever centring leaves after the bar's
/// height is decided elsewhere. Trimming the margin is the smallest change
/// that makes the three gaps equal without touching the bar's height.
const TRIM: &str = ".probe headerbar { min-height: 30px; padding: 0 6px; } \
                    .probe headerbar windowcontrols button { \
                        min-width: 24px; min-height: 24px; padding: 0; \
                        margin: 2px 1px 2px 2px; \
                    }";

/// The candidate that stops relying on centring. `windowhandle` is the node
/// that actually insets the bar's contents -- measured, not assumed: it spans
/// the full width while its `GtkCenterBox` child sits inset by an equal amount
/// each side. Give it one uniform padding and zero everything that adds to it,
/// and all four gaps are that padding by construction, rather than the right
/// one being a sum and the vertical ones being whatever centring leaves over.
const UNIFORM: &str = ".probe headerbar { min-height: 0; padding: 0; } \
                       .probe headerbar windowhandle { padding: 4px; } \
                       .probe headerbar windowcontrols button { \
                           min-width: 24px; min-height: 24px; padding: 0; margin: 0; \
                       }";

/// Third candidate: the inset lives on `windowhandle`, not `headerbar`.
const WH0: &str = ".probe headerbar windowhandle { padding: 0; } \
                   .probe headerbar windowcontrols button { \
                       min-width: 24px; min-height: 24px; padding: 0; margin: 2px; \
                   }";

/// Fourth: same place, trimmed by one so right matches the centred vertical.
const WH6: &str = ".probe headerbar { min-height: 30px; } \
                   .probe headerbar windowhandle { padding: 0 6px; } \
                   .probe headerbar windowcontrols button { \
                       min-width: 24px; min-height: 24px; padding: 0; margin: 2px; \
                   }";

/// Second candidate: put the whole inset on the button and none on the bar.
const ZERO: &str = ".probe headerbar { min-height: 30px; padding: 0; } \
                    .probe headerbar windowcontrols button { \
                        min-width: 24px; min-height: 24px; padding: 0; margin: 8px; \
                    }";

/// Depth-first walk for the close button. `windowcontrols` builds its children
/// itself, so there is no handle to hold onto. The distinguishing mark is the
/// `close` style class, not the icon name -- the button carries a `GtkImage`
/// child rather than an icon-name property, so `icon_name()` reads empty on
/// all three controls and matching on it finds nothing.
fn find_close(w: &gtk::Widget) -> Option<gtk::Button> {
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        if b.has_css_class("close") {
            return Some(b.clone());
        }
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(found) = find_close(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

fn main() {
    let app = adw::Application::builder()
        .application_id("io.github.luohoa97.CompactBarProbe")
        .build();

    app.connect_activate(|app| {
        let which = std::env::var("CORDIAL_PROBE_SHEET").unwrap_or_else(|_| "new".into());
        let sheet = match which.as_str() {
            "old" => OLD,
            "zero" => ZERO,
            "trim" => TRIM,
            "uniform" => UNIFORM,
            "wh0" => WH0,
            "wh6" => WH6,
            _ => NEW,
        };

        let css = gtk::CssProvider::new();
        css.load_from_string(sheet);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(600)
            .default_height(200)
            .build();
        window.add_css_class("probe");

        gtk::style_context_add_provider_for_display(
            &WidgetExt::display(&window),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let header = adw::HeaderBar::new();
        let content = adw::ToolbarView::new();
        content.add_top_bar(&header);
        content.set_content(Some(&gtk::DrawingArea::new()));
        window.set_content(Some(&content));

        // Allocations are meaningless until the frame clock has laid the window
        // out at least once, so this samples after a tick rather than on map.
        let w = window.clone();
        let h = header.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
            let bar_h = h.height();
            match find_close(h.upcast_ref::<gtk::Widget>()) {
                Some(btn) => {
                    let (bw, bh) = (btn.width(), btn.height());
                    // Button origin in the header bar's own coordinates.
                    let p = btn
                        .compute_point(&h, &gtk::graphene::Point::new(0.0, 0.0))
                        .expect("close button and header bar share a root");
                    let (x, y) = (p.x().round() as i32, p.y().round() as i32);
                    let right = h.width() - (x + bw);
                    let bottom = bar_h - (y + bh);
                    println!("sheet             : {which}");
                    println!("header bar        : {}x{}", h.width(), bar_h);
                    println!("close button      : {bw}x{bh} at ({x},{y})");
                    println!("gap top           : {y}");
                    println!("gap bottom        : {bottom}");
                    println!("gap right         : {right}");
                    println!(
                        "equal?            : {}",
                        if y == bottom && bottom == right { "YES" } else { "NO" }
                    );
                    // Walk up from the button naming every ancestor's right
                    // edge, so the slack can be attributed to one widget
                    // instead of guessed at from the stylesheet.
                    println!("-- right edge by ancestor --");
                    let mut cur: Option<gtk::Widget> = Some(btn.clone().upcast());
                    while let Some(node) = cur {
                        let pt = node
                            .compute_point(&h, &gtk::graphene::Point::new(0.0, 0.0))
                            .expect("shares a root");
                        println!(
                            "  {:22} x={:4} w={:4} right_edge={:4}",
                            node.type_().name(),
                            pt.x().round() as i32,
                            node.width(),
                            pt.x().round() as i32 + node.width()
                        );
                        if node.type_().name() == "AdwHeaderBar" { break; }
                        cur = node.parent();
                    }
                }
                None => {
                    println!("close button not found; tree follows");
                    fn dump(w: &gtk::Widget, d: usize) {
                        let icon = w
                            .downcast_ref::<gtk::Button>()
                            .and_then(|b| b.icon_name())
                            .map(|g| g.to_string())
                            .unwrap_or_default();
                        println!(
                            "{:indent$}{} css={:?} icon={:?} classes={:?}",
                            "",
                            w.type_().name(),
                            w.css_name(),
                            icon,
                            w.css_classes(),
                            indent = d * 2
                        );
                        let mut c = w.first_child();
                        while let Some(k) = c {
                            dump(&k, d + 1);
                            c = k.next_sibling();
                        }
                    }
                    dump(h.upcast_ref::<gtk::Widget>(), 0);
                }
            }
            w.close();
        });

        window.present();
    });

    app.run();
}
