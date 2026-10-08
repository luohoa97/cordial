use cordial_shell::{host_window::HostWindow, title_bar::TitleBar};
use libadwaita::prelude::*;
use std::time::Duration;

#[test]
#[ignore = "requires a Wayland display and CORDIAL_TITLE_BAR=hidden"]
fn hidden_title_bar_only_changes_mapped_game_window() {
    // Given real launcher and game windows in a process launched with Hidden.
    libadwaita::init().unwrap();
    let choice = TitleBar::from_env();
    assert_eq!(choice, TitleBar::Hidden, "launch this test with CORDIAL_TITLE_BAR=hidden");
    let launcher_content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    let launcher = HostWindow::new("Cordial launcher fixture", 640, 480, &launcher_content);
    let game = HostWindow::with_canvas("Cordial game fixture", 640, 480);

    // When both windows are mapped normally, without asking for fullscreen.
    launcher.present();
    game.present();
    launcher.wait_until_mapped(Duration::from_secs(5)).unwrap();
    game.wait_until_mapped(Duration::from_secs(5)).unwrap();

    // Then Hidden removes only game chrome; launcher controls remain available.
    assert!(!launcher.window().is_fullscreen());
    assert!(!game.window().is_fullscreen());
    assert!(launcher.toolbar().reveals_top_bars());
    assert!(launcher.toolbar().top_bar_height() > 0);
    assert!(!game.toolbar().reveals_top_bars());
    assert_eq!(game.toolbar().top_bar_height(), 0);
    println!(
        "title-bar fixture: mode={choice:?}, launcher_top_bar_height={}, game_top_bar_height={}",
        launcher.toolbar().top_bar_height(),
        game.toolbar().top_bar_height()
    );
    launcher.window().close();
    game.window().close();
}

/// Run GTK's main loop for `ms`, which is how an animated reveal is let finish.
fn spin(ms: u64) {
    let end = std::time::Instant::now() + Duration::from_millis(ms);
    let ctx = libadwaita::glib::MainContext::default();
    while std::time::Instant::now() < end {
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The live change of ADR-044: a window that is already up takes a different
/// title bar without being rebuilt. Launched with the default, so every
/// assertion below is about a change and not about the starting state.
#[test]
#[ignore = "requires a Wayland display; launch without CORDIAL_TITLE_BAR"]
fn the_title_bar_can_change_while_the_game_window_is_up() {
    libadwaita::init().unwrap();
    assert_eq!(TitleBar::from_env(), TitleBar::Default, "launch this test without CORDIAL_TITLE_BAR");
    let game = HostWindow::with_canvas("Cordial live title bar fixture", 640, 480);
    game.present();
    game.wait_until_mapped(Duration::from_secs(5)).unwrap();
    spin(400);

    // Starting point: the ordinary bar.
    assert!(game.toolbar().reveals_top_bars());
    let ordinary = game.toolbar().top_bar_height();
    assert!(ordinary > 0);
    assert_eq!(game.title_bar(), TitleBar::Default);

    // Hidden takes it away, and the space with it.
    game.set_title_bar(TitleBar::Hidden);
    spin(800);
    assert_eq!(game.title_bar(), TitleBar::Hidden);
    assert!(!game.toolbar().reveals_top_bars());
    assert_eq!(game.toolbar().top_bar_height(), 0);

    // Compact brings it back shorter than the ordinary one; the stylesheet is
    // what makes the difference, so this is the only place it is measured.
    game.set_title_bar(TitleBar::Compact);
    spin(800);
    assert!(game.toolbar().reveals_top_bars());
    let compact = game.toolbar().top_bar_height();
    assert!(compact > 0 && compact < ordinary, "compact {compact} should be shorter than {ordinary}");

    // Control: back to the default restores exactly the height it started at,
    // so the compact sheet was removed and not merely outvoted.
    game.set_title_bar(TitleBar::Default);
    spin(800);
    assert_eq!(game.toolbar().top_bar_height(), ordinary);
    println!("title-bar live: default={ordinary} hidden=0 compact={compact} default-again={ordinary}");
    game.window().close();
}

/// The window keeps one row the engine's rectangle does not cover exactly when
/// no header bar is showing (ADR-056). The content rectangle is what the engine
/// is sized from, so it is the thing to measure: with the bar up it is the
/// window less the bar, and with the bar gone it must be the window less one
/// row, not the whole window. Without that row a compositor that culls hidden
/// surfaces sends the GTK surface no frame callbacks, and the editor never
/// shows. Launched with the default title bar, so every assertion is about a
/// change.
#[test]
#[ignore = "requires a Wayland display; launch without CORDIAL_TITLE_BAR"]
fn the_window_keeps_one_row_the_engine_does_not_cover_when_no_bar_shows() {
    libadwaita::init().unwrap();
    assert_eq!(TitleBar::from_env(), TitleBar::Default, "launch this test without CORDIAL_TITLE_BAR");
    let game = HostWindow::with_canvas("Cordial visibility anchor fixture", 640, 480);
    game.present();
    game.wait_until_mapped(Duration::from_secs(5)).unwrap();
    spin(600);

    // With the bar up the bar itself is the uncovered part: nothing is added.
    let bar = game.toolbar().top_bar_height();
    assert!(bar > 0);
    let (_, _, _, h_bar) = game.content_rect().unwrap();
    assert_eq!(h_bar + bar, game.window().height(), "bar up: content plus bar is the window");

    // With it gone, one row is left over.
    game.set_title_bar(TitleBar::Hidden);
    spin(800);
    let (_, _, _, h_hidden) = game.content_rect().unwrap();
    assert_eq!(h_hidden + 1, game.window().height(), "bar hidden: content plus one row is the window");

    // And it goes again when the bar comes back, so the strip is not a
    // permanent loss in the ordinary window.
    game.set_title_bar(TitleBar::Default);
    spin(800);
    let (_, _, _, h_back) = game.content_rect().unwrap();
    assert_eq!(h_back + bar, game.window().height(), "bar back: the strip is gone");
    println!("visibility anchor: bar {bar}, content {h_bar} / {h_hidden} / {h_back}, window {}", game.window().height());
    game.window().close();
}
