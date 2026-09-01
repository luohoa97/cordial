//! Turning Roblox ids into the name and pictures a presence needs.
//!
//! **Why Cordial makes these requests at all.** A Discord activity carries an
//! image as a URL, and neither an experience nor Cordial knows the URL: what
//! BloxstrapRPC sends is a Roblox asset id, and what the engine's log names on
//! a join is a universe id. Only Roblox can turn either into a picture, so
//! something has to ask it. Bloxstrap asks the same two services from
//! `UniverseDetails`, and the endpoints and their query strings here are its
//! (MIT, Bloxstrap Labs) -- the shape adapted, not the implementation.
//!
//! This is a narrower disclosure than it first looks. Every request here names
//! a place the player has *already joined*, to Roblox, while connected to
//! Roblox, from a client Roblox is talking to anyway. That is the test
//! `SessionState`'s module comment applies when it refuses to look up a
//! username: the objection there is a call made "for a plugin's convenience"
//! that "a third party would see", and neither half holds for asking Roblox
//! about the game you are visibly in. Nothing here is called unless a presence
//! plugin is running and the player is in a game.
//!
//! **Everything fails soft.** A presence with no picture is worth having; a
//! presence delayed behind a hung HTTP request is not, and one that panics is
//! worse than either. Every function answers `None` on any failure and says so
//! once rather than per retry.

use std::collections::HashMap;
use std::sync::Mutex;

/// What the games API says about an experience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameDetails {
    pub name: String,
    pub creator: String,
}

/// Resolved answers, kept for the life of the process.
///
/// **Cached because the alternative is a request every twenty seconds.** The
/// presence heartbeat re-sends the whole activity on a timer, and re-resolving
/// an id that cannot have changed would turn one lookup into a hundred and
/// eighty an hour. A negative answer is cached too -- an id Roblox will not
/// resolve now will not resolve on the next heartbeat either, and retrying it
/// forever is how a soft failure becomes a hard one.
static CACHE: Mutex<Option<Caches>> = Mutex::new(None);

#[derive(Default)]
struct Caches {
    assets: HashMap<String, Option<String>>,
    icons: HashMap<u64, Option<String>>,
    details: HashMap<u64, Option<GameDetails>>,
}

fn with_cache<T>(f: impl FnOnce(&mut Caches) -> T) -> T {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Caches::default))
}

/// The CDN URL for one Roblox asset id, for a picture a game asked for.
///
/// **The endpoint matters and the obvious one is wrong.**
/// `www.roblox.com/asset-thumbnail/image?assetId=...` reads like the answer and
/// returns **404**; it was shipped here on the strength of Discord *accepting*
/// the URL, which it does for any URL at all -- it rewrites whatever it is
/// given into its `mp:external/...` proxy form without fetching it. The result
/// was a broken-image placeholder where Cordial's own icon had been, which is
/// worse than the bug it replaced. Accepting is not fetching, and the check
/// that settles it is `curl` against the URL, not the reply from Discord.
pub fn asset_image_url(asset_id: &str) -> Option<String> {
    if asset_id.is_empty() || !asset_id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if let Some(hit) = with_cache(|c| c.assets.get(asset_id).cloned()) {
        return hit;
    }
    let url = format!(
        "https://thumbnails.roblox.com/v1/assets?assetIds={asset_id}\
         &size=420x420&format=Png&isCircular=false"
    );
    let resolved = first_image_url(&url, "asset");
    with_cache(|c| c.assets.insert(asset_id.to_string(), resolved.clone()));
    resolved
}

/// The experience's own icon, which is Cordial's default picture for a game.
///
/// Query string is Bloxstrap's, `returnPolicy=PlaceHolder` included: that asks
/// Roblox for its own placeholder rather than an empty answer for a game whose
/// icon is still moderating, so a new experience shows *something*.
pub fn game_icon_url(universe_id: u64) -> Option<String> {
    if let Some(hit) = with_cache(|c| c.icons.get(&universe_id).cloned()) {
        return hit;
    }
    let url = format!(
        "https://thumbnails.roblox.com/v1/games/icons?universeIds={universe_id}\
         &returnPolicy=PlaceHolder&size=512x512&format=Png&isCircular=false"
    );
    let resolved = first_image_url(&url, "game icon");
    with_cache(|c| c.icons.insert(universe_id, resolved.clone()));
    resolved
}

/// The experience's name and who made it.
pub fn game_details(universe_id: u64) -> Option<GameDetails> {
    if let Some(hit) = with_cache(|c| c.details.get(&universe_id).cloned()) {
        return hit;
    }
    let url = format!("https://games.roblox.com/v1/games?universeIds={universe_id}");
    let resolved = fetch_json(&url, "game details").and_then(|v| {
        let entry = v.get("data")?.as_array()?.first()?;
        Some(GameDetails {
            name: entry.get("name")?.as_str()?.to_string(),
            creator: entry
                .get("creator")
                .and_then(|c| c.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    });
    with_cache(|c| c.details.insert(universe_id, resolved.clone()));
    resolved
}

/// Both thumbnail endpoints answer the same envelope: `data[0].imageUrl`, with
/// a `state` that is `Completed` only when there is really a picture.
fn first_image_url(url: &str, what: &str) -> Option<String> {
    let value = fetch_json(url, what)?;
    let entry = value.get("data")?.as_array()?.first()?;
    // A thumbnail that is still rendering or was moderated answers with a
    // state and no usable URL. Taking the URL regardless is how you end up
    // publishing an empty string as an image.
    if entry.get("state").and_then(|s| s.as_str()) != Some("Completed") {
        return None;
    }
    let image = entry.get("imageUrl")?.as_str()?;
    if image.is_empty() {
        return None;
    }
    Some(image.to_string())
}

/// **The house client, not a second one.** `cordial_update::http::get_text`
/// is what this project already uses for every Roblox metadata fetch, and
/// `client_settings` reuses it for the same stated reason: its connect and
/// total timeouts were chosen for exactly this shape of request and a second
/// set here would drift apart from them. It also carries `url_policy`'s
/// host-locked redirect handling, which a bare `ureq::get` does not.
///
/// These calls are made off the poll thread by the caller, which is what makes
/// a twenty-second worst case acceptable -- see `game_log`.
fn fetch_json(url: &str, what: &str) -> Option<serde_json::Value> {
    match cordial_update::http::get_text(url) {
        Ok(body) => match serde_json::from_str(&body) {
            Ok(value) => Some(value),
            Err(e) => {
                println!("  presence: the {what} lookup returned something unreadable: {e}");
                None
            }
        },
        Err(e) => {
            println!("  presence: could not look up the {what}: {e}");
            None
        }
    }
}

/// Whether a string is a Roblox CDN image URL.
///
/// **The guard that lets a URL cross the plugin boundary at all.** Cordial
/// resolves ids here, in the runtime, because this is the crate with an HTTP
/// client -- so what reaches the presence broker is a URL rather than an id,
/// and a URL from a plugin is exactly the "publish an arbitrary link under
/// Cordial's name and icon" that the buttons are guarded against. Restricting
/// it to Roblox's own CDN keeps that guarantee: the worst a plugin can do is
/// show a different Roblox picture.
pub fn is_roblox_image_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    // A suffix test alone would accept `evil-rbxcdn.com`, so the dot is part
    // of the pattern and a bare `rbxcdn.com` is allowed on its own.
    (host == "rbxcdn.com" || host.ends_with(".rbxcdn.com"))
        && !host.contains('@')
        && !host.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_roblox_cdn_url_is_accepted() {
        assert!(is_roblox_image_url(
            "https://tr.rbxcdn.com/180DAY-39d551a26b403913451abaa2ca1ad9b9/420/420/Image/Png/noFilter"
        ));
        assert!(is_roblox_image_url("https://rbxcdn.com/x.png"));
    }

    /// **A look-alike host must not pass.** This is the whole value of the
    /// guard: it is what stops a plugin turning a resolved-image field back
    /// into an arbitrary link under Cordial's name.
    #[test]
    fn a_lookalike_host_is_refused() {
        for bad in [
            "https://evil-rbxcdn.com/x.png",
            "https://rbxcdn.com.evil.test/x.png",
            "http://tr.rbxcdn.com/x.png",
            "https://user@evil.test/x.png",
            "https://example.test/x.png",
            "tr.rbxcdn.com/x.png",
            "",
        ] {
            assert!(!is_roblox_image_url(bad), "{bad:?} must be refused");
        }
    }

    /// An asset id is validated before it becomes part of a URL, so a
    /// malformed one costs no request at all.
    #[test]
    fn a_non_numeric_asset_id_is_not_even_looked_up() {
        assert_eq!(asset_image_url(""), None);
        assert_eq!(asset_image_url("../../etc"), None);
        assert_eq!(asset_image_url("1 OR 1"), None);
    }
}

/// Cordial's key for a Roblox asset's picture.
///
/// Prefixed so an asset id and a universe id cannot collide in the one map the
/// presence broker looks pictures up in -- they are separate id spaces and
/// `13913198647` could plausibly be both.
pub fn asset_key(asset_id: &str) -> String {
    format!("a{asset_id}")
}

/// Cordial's key for an experience's own icon.
pub fn universe_key(universe_id: u64) -> String {
    format!("u{universe_id}")
}

/// Resolve an experience's name, creator and icon, and register the icon.
///
/// **Blocking, and called from a thread of the caller's making.** Three HTTP
/// requests behind the log-watcher's poll loop would stall the watcher for as
/// long as Roblox took to answer; `game_log` spawns for this and republishes
/// when it returns.
pub fn resolve_game(universe_id: u64) -> Option<GameDetails> {
    if let Some(icon) = game_icon_url(universe_id) {
        cordial_plugins::presence::remember_image(&universe_key(universe_id), &icon);
    }
    game_details(universe_id)
}

/// Resolve one asset a game asked for, and register it.
pub fn resolve_asset(asset_id: &str) {
    if let Some(url) = asset_image_url(asset_id) {
        cordial_plugins::presence::remember_image(&asset_key(asset_id), &url);
    }
}
