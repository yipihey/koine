# The theme: a default look that reads as an app

Planned 2026-10-14, after the tour (docs/plan-koine-tour.md) put every
part of the grammar on an iPhone and the page around it still read as a
1990s web page: a Tailwind blue, grey-on-white secondary text, underlined
router links in a strip, browser-default buttons. None of that is native
and none of it is a surface's: it is the page's own defaults, in two CSS
strings and one stylesheet, which is why one theme fixes the demo, every
bespoke app and the browser at `/ui/` at once.

## Decisions

### TH-D1. One default theme, Apple's conventions, in CSS only

The page's default look follows the platform it draws on through CSS the
platform already understands: `color-scheme: light dark`, the system
colors (`Canvas`, `CanvasText`, `LinkText` where they fit) with the iOS
and macOS palette for what they do not name (the blue `#007aff` /
`#0a84ff` accent, label and secondary-label greys, grouped backgrounds,
hairline separators), the `-apple-system` font stack with Dynamic-Type
sizes, and the safe-area insets. Dark mode is automatic. On a non-Apple
platform the same theme falls back to `system-ui` and the same tokens: a
clean app look, not a broken one. No Swift, no second renderer, no change
to any surface spec. The koine style slots (`crates/koine/src/style.rs`,
the `/style` page) keep working: the theme defines the `--koine-color-*`
and spacing tokens from the system palette, and a slot edit still
overrides them.

### TH-D2. Navigation fits the platform: a tab bar below, a toolbar above

Below 700 px or with a coarse pointer the four routes are a bottom tab
bar with icons and labels, pinned above the home indicator; above that
they are a top toolbar with the same icons, the way a Mac app has one.
The underlined links are gone. Page titles are large titles. The status
line stops being a permanent strip: the origin and token go to the Host
page, and a banner appears only when there is a refusal to show.

### TH-D3. Grouped inset lists, 44 px rows, hairlines, chevrons

The Home list, the Host page's pairs, and the surfaces' tables, lists
and trees take the grouped inset style: rounded cards on the grouped
background, hairline separators, a chevron on a row that navigates,
44 px rows and tap targets, `-webkit-tap-highlight-color: transparent`
with a pressed state of its own.

### TH-D4. Controls that look like controls

Filled or tinted pill buttons (the primary action filled, the rest
tinted), inset rounded text fields, system sliders and toggles
(`-webkit-appearance` where WebKit draws them well, a CSS switch for the
toggle), a segmented look for tabs, status banners in the system colors,
code and logs in the system monospace on a grouped card that wraps and
scrolls rather than overflowing the page.

### TH-D5. Proven by screenshots, not by reading CSS

A theme cannot be judged from Linux by reading it. The proof is the
browser bundle (`dx build --platform web`) served by `koine-host
--web-dir` over the tour's five surfaces, screenshotted by Playwright's
Chromium at an iPhone viewport (390×844, device scale 3, light and dark)
and a Mac one (1280×800), on every page and every tour surface, with the
images kept under `docs/screenshots/` for the next person. The phone
confirms what WebKit does differently.

## Status

Landed 2026-10-14 on branch `theme`: TH1 (the tokens in `koine.css`, the
`Nav` tab bar and toolbar, the refusal banner, large titles, grouped
cards, pill buttons, inset fields, the switch, the wrapping log and code
cards) and TH2 (`scripts/screenshots.sh` + `scripts/screenshots.mjs`;
`docs/screenshots/*.png`, 27 images: every page and every tour surface at
390×844 light and dark and 1280×800). Beside the theme: the surface page
shows the surface's name as its title (the tour's specs no longer carry a
"# Title" text node, and their descriptions are plain text since the web
text node draws markdown literally), the Home rows show tags rather than
ids, and an iPhone's device tag is `iphone` (there is no `hostname` to
run). Known and left: the plot SVG paints its own white background, so in
dark mode it is a white card inside a dark one (koine-plot's, a later
wave: plots that follow the theme); the map graph's labels overlap
(the graph layout's); Linux Chromium draws DejaVu where a device draws
SF. Release `demo-v0.3.0` is the phone's judgement.
