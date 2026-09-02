//! Fetching the official Roblox Android build, at the user's request.
//!
//! [ADR-015](../../../docs/adr/ADR-015-fetching-the-roblox-build.md) is the
//! decision this implements and it draws the line in one sentence: Cordial may
//! **download** the official build from Roblox's own distribution to the user's
//! own machine, and may never **ship** one — nothing committed, vendored,
//! bundled in a release artefact, served to a third party, or modified on the
//! way through. Nothing in this crate writes a Roblox byte anywhere but the
//! user's cache, and nothing in it re-serves one.
//!
//! The parts, and which of them the settings govern:
//!
//! [`version`] asks Roblox what the current build is. It is one request and it
//! runs in the background after the window is up — synchronously is the mistake,
//! because then a slow or absent network delays the window rather than the
//! answer. [`changelog`] fetches the release notes so the header-bar button has
//! something to show. Neither is governed by anything: they are cheap.
//! [`engine`] answers the other half of the same question without a network at
//! all, by reading the installed engine's version out of `libroblox.so`.
//!
//! [`download`] is the expensive half and the only part [`settings`] governs,
//! together with [`metered`], which asks NetworkManager whether this connection
//! is one somebody pays for by the megabyte. [`apk`] applies ADR-014's
//! extraction refusals to what arrived, [`install`] owns the directory the
//! result goes into and the order that keeps a working build working, and
//! [`cache`] stamps the extracted engine with the APK it came from so a new
//! build re-extracts and an unchanged one does not.
//!
//! ## What was measured, and when
//!
//! Every URL below was requested from this machine on 2026-08-02, and two of the
//! answers are not what the design assumed. They are recorded in the module that
//! owns each URL, and in `docs/design/updating-roblox.md`, because ADR-015
//! accepts that this is a maintenance surface and the only thing that makes a
//! maintenance surface maintainable is knowing what it looked like when it
//! worked.
//!
//! The short version: Roblox serves a version for `WindowsPlayer`, `MacPlayer`
//! and `WindowsStudio64`, and answers HTTP 500 for `AndroidApp`. There is no
//! Roblox-hosted download for the Android build that this crate could find, so
//! it ships no URL for one and says so rather than guessing at a shape.
//!
//! **Re-measured 2026-08-20**, with `cargo run -p cordial-update --example
//! update_probe`, and every one of those answers still holds:
//!
//! ```text
//! AndroidApp                                  500, same body
//! WindowsPlayer                               200, 0.735.0.7351131
//! setup.rbxcdn.com/DeployHistory.txt          200, 7230 lines, no android/apk
//! setup.rbxcdn.com/android/DeployHistory.txt  403 AccessDenied
//! newest release notes                        Release Notes for 734
//! engine in this cache                        2.734.0.917
//! ```
//!
//! The last two lines are the ones that changed what this crate can do. They are
//! the same number, from two places that have never been compared before,
//! because until [`engine`] existed the second one could not be read.
//!
//! ## Failure names what it could not reach
//!
//! Every fallible entry point here returns a value that carries the URL, the
//! bus name or the path it could not get an answer from. ADR-015 requires it in
//! as many words, and AGENTS.md's rule about stubs is the general case: a
//! fetcher that reports success on a distribution URL Roblox has moved is worse
//! than one that reports failure, because the user then debugs a Roblox build
//! that is quietly six months old.

pub mod apk;
pub mod apk_signature;
pub mod cache;
pub mod changelog;
pub mod deno;
pub mod download;
pub mod engine;
pub mod install;
pub mod metered;
pub mod provider;
pub mod settings;
pub mod url_policy;
pub mod version;

/// Public so the probe can ask a URL this crate deliberately does *not* fetch
/// from — Roblox's deployment CDN — with Cordial's own truthful user agent
/// rather than a second ad-hoc client built beside it. That the CDN has no
/// Android path is the claim [`download::Source::official`] rests on, and a
/// claim nothing re-measures is a claim that decays without anybody noticing.
pub mod http;

mod sha256;

pub use sha256::Sha256Hash;

/// Why something Cordial had to reach did not answer.
///
/// One type for every network call in this crate, because the three ways a
/// fetch fails want telling apart by whoever reads the message: nothing
/// answered, something answered with a refusal, or something answered with a
/// body that was not what this code knows how to read. The URL is in all three,
/// which is the requirement ADR-015 states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unreachable {
    /// Nothing answered: DNS, TLS, connection, timeout.
    Transport { url: String, why: String },
    /// Something answered, and it was not a success.
    Status { url: String, status: u16, body: String },
    /// Something answered successfully with a body this code cannot read.
    /// Distinct from [`Unreachable::Status`] on purpose: a 200 whose shape
    /// changed is Roblox having moved something, which is the failure ADR-015
    /// says must not present as "no update available".
    Malformed { url: String, why: String },
    /// The archive arrived intact and was refused.
    ///
    /// **Distinct from every other variant here on purpose.** The rest describe
    /// a network that did not deliver; this one describes a delivery that was
    /// rejected, which is the single outcome ADR-025 exists to produce and the
    /// one a user must not read as "try again in a minute". Reported through
    /// `Malformed` it looked like a transport glitch, which is the most
    /// misleading possible framing for "somebody served bytes Roblox did not
    /// sign".
    Refused { what: String, why: String },
    /// The user asked to stop. Not a failure: nothing is wrong, and the
    /// message must not read as though something is.
    Cancelled,
    /// There was nothing to reach. A source that needs no network still fails
    /// -- the local build is simply absent -- and folding that into a transport
    /// error would put a URL in a message about a missing file.
    NoSource { why: String },
}

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unreachable::Refused { what, why } => write!(
                f,
                "The downloaded build was refused rather than installed: {why} ({what}). \
                 Nothing was installed and the build you have is untouched."
            ),
            Unreachable::Cancelled => write!(f, "Download stopped."),
            Unreachable::NoSource { why } => write!(f, "{why}"),
            Unreachable::Transport { url, why } => write!(f, "could not reach {url}: {why}"),
            Unreachable::Status { url, status, body } => {
                let body = body.trim();
                if body.is_empty() {
                    write!(f, "{url} answered HTTP {status}")
                } else {
                    write!(f, "{url} answered HTTP {status}: {}", first_line(body))
                }
            }
            Unreachable::Malformed { url, why } => {
                write!(f, "{url} sent a reply Cordial could not read: {why}")
            }
        }
    }
}

impl std::error::Error for Unreachable {}

/// One line of a server's body, capped.
///
/// An error page is occasionally an entire HTML document, and a message that
/// pastes one into a dialog is a message nobody reads.
fn first_line(body: &str) -> String {
    let line = body.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}
