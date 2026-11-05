# Decisions

- 2026-11-05: Base is Textual 4f09312 (Michael Morris's last commit), not the Textual 8 work in progress. Decided by Michael.
- 2026-11-05: Rust, gtk4-rs, libadwaita and WebKitGTK 6.0. Decided by Michael.
- 2026-11-05: Changes go straight to `main` after an independent subagent review. No PRs, because Michael does not want to close them. Decided by Michael ("review everything yourself").
- 2026-11-05: WebKitGTK's sandbox stays on. On Ubuntu 24.04 it needs an AppArmor profile that allows user namespaces (`packaging/apparmor/textual-gtk-dev`, the same shape as Ubuntu's epiphany profile). Michael loaded it on koya. A Flatpak needs nothing extra; a `.deb` would ship its own profile.
- 2026-11-05: A small template engine (`src/style/template.rs`) instead of a Mustache crate. Both `mustache` and `ramhorns` fail to parse `formattedMessageFragment`, which every message goes through, because it uses GRMustache's else clause. Details in `docs/research/templates.md`.
- 2026-11-05: `resources/styling` is upstream's `Sources/App/Resources/Styling`, copied unchanged, with the BSD notices in `resources/styling/LICENSE.md`.
- 2026-11-05: One deliberate rendering difference from Textual: a colour span still open on a message's last fragment is closed, so the HTML nests. Textual leaves it open in some cases; the result looks the same.
- 2026-11-05 (default, Michael can override): the name stays "textual-gtk" for now. The application ID is `io.github.androidkitkat.TextualGtk` until a name is chosen.
- 2026-11-05 (default, Michael can override): OTR is not decided until phase 3. Nothing in phases 1 and 2 depends on it.
- 2026-11-05 (default, Michael can override): keep Textual's structure and behaviour, drawn with libadwaita (header bar, Adwaita widgets) rather than copying macOS chrome.
- 2026-11-05: libadwaita stays, plus an appearance setting "Textual for Mac" that loads an extra stylesheet restyling the window chrome (sidebar, server list, member list, input bar) to look like the macOS app. Decided by Michael. Phase-2 widgets get stable CSS class names so the stylesheet needs no code changes; the setting itself lands with the preferences window in phase 3, matched against screenshots of the Mac app.
