# Branding assets

The open G and solid diamond are the shared brand mark. Blue comes from the app's accent
palette; the wordmark uses an even weight. The assistant's animated radar remains an activity
indicator, not a second product logo.

| File | Used by | Spec |
|---|---|---|
| `logo.svg` | README header and in-app brand marks | Canonical 256×256 transparent SVG; solid terminal for small-size clarity |
| `logo-horizontal.svg` | Light-background headers | 640×192 transparent lockup, blue mark and ink wordmark |
| `logo-horizontal-dark.svg` | Dark-background headers | Same geometry with the dark-theme blue accent and an off-white wordmark |
| `screenshot.webp` | README hero | ~2× of an 860 px display width (current: 2548×1640); a chat showing streaming + an MCP approval card |
| `demo.gif` *(optional)* | README hero, instead of or alongside the screenshot | ≤ 10 MB, ≤ 20 s. GitHub only embeds MP4s uploaded through the web editor, not repo-relative paths |
| `social-preview.png` | Repo **Settings → General → Social preview** (upload manually) | 1280×640 PNG, under 1 MB; logo, tagline, and product positioning |
| `social-preview.svg` | Social preview vector export | Outlined text; generated alongside the PNG |

An optional demo GIF or MP4 can be added alongside the screenshot. After adding one, link it
from the root README (for GitHub-hosted video, drag the file into the web editor to get an
embeddable URL).

## Regeneration

Run `npm run gen:brand` after editing `logo.svg` or changing the display name in `product.json`.
This requires the installed npm dependencies, **Inkscape**, and **Arial** for the source
wordmark and social typography. Inkscape is found on PATH or in its standard macOS app location;
set `INKSCAPE_BIN` to override it. The committed exports have outlined text, so viewers do not
need the font. Normal app builds do not require Inkscape or regenerate these assets.

The script generates both horizontal lockups, the social preview, and `src-tauri/icons/source.svg`.
It then runs the project's installed Tauri icon CLI and copies only desktop PNG/ICO/ICNS assets.
The desktop tile has transparent outer padding, an ink surface, and the same mark with a subtle
blue gradient. Do not edit individual raster sizes; regenerate them together.

`BrandMark.svelte` masks the canonical SVG with the current theme's accent, including the
prefixed mask property for Safari 15.6. The sidebar wordmark remains live text sourced from
`product.json`. The assistant radar keeps its existing animation and reduced-motion behavior.
