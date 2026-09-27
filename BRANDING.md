# Casita branding

Approved direction: **Root House with the lowercase Casita companion wordmark**
from Obrador, with Rust-orange accents (September 2026). Use this identity for
the docs and future Casita surfaces.

## Logo

The Rust-orange house contains a transparent Merkle tree: one circular root,
two circular children, and smooth symmetric branches. The wordmark reads
**casita** in lowercase charcoal lettering (near-white on dark backgrounds).
The horizontal logo uses the compact house artwork so its tree remains clear
at small sizes. Preserve the exported proportions, spacing, and cutouts.

The wordmark uses outlined Righteous Regular lettering, matching the Casita
companion logo in Obrador's build diagram. The SVGs come from the Obrador
repository's `docs/public/brand/casita-logo.svg` and `casita-logo-dark.svg`.
Righteous Regular is by Astigmatic; Obrador bundles its SIL Open Font License
at `design/brand/fonts/Righteous-OFL.txt`.
Use the outlined SVGs rather than approximating the wordmark with a font.

Canonical assets live in `docs/public/brand/`:

- `logo.svg`: primary horizontal logo.
- `logo-dark.svg`: reversed logo for dark backgrounds.
- `logo-mono.svg` and `logo-mono-dark.svg`: single-color horizontal logos.
- `mark.svg` and `mark-dark.svg`: standalone detailed house.

`docs/public/favicon.svg` uses the compact house from the horizontal logo,
with the matching Rust-orange color for light and dark browser themes.

Preserve the SVGs' clear space and intrinsic aspect ratio. Do not crop,
stretch, add shadows, or place them inside another badge. Use the standalone
house when the full wordmark would be too small. The earlier `casita.png`
and uppercase wordmark are not the current identity.

## Color

| Role | Color |
| --- | --- |
| Rust orange, primary logo and filled buttons | `#CE422B` |
| Rust orange on dark backgrounds | `#EF8664` |
| Accessible text accent on white | `#B53724` |
| Charcoal | `#17191E` |
| White | `#FFFFFF` |
| Dark-background logo lettering | `#FBFBFB` |
| Muted light surface | `#F5F6F8` |
| Muted dark surface | `#202228` |

Rust is the brand accent. Blue, green, and amber may distinguish technical
diagram categories or warning/success states, but are not brand substitutes.
Use restrained tint backgrounds, fine borders, and generous space rather
than decorative gradients or glows.

## Typography and voice

Use Manrope for prose and headings and DM Mono for code and small labels,
with system fallbacks. Use bold, closely spaced headings, readable prose,
and compact uppercase section labels. The outlined logo is always vector artwork.

The brand line is **“A home for your objects.”** Explain it concretely:
files, Git objects, IPLD blocks, and custom graphs share verified storage,
synchronization, and garbage collection. Keep pre-release and experimental
capability boundaries explicit. Prefer precise examples to broad promises.

## Implementation

`docs/src/styles/custom.css` owns the shared tokens and reading experience.
`docs/src/styles/landing.css` owns homepage layout. `BrandLogo.astro` handles
theme-aware artwork. Preserve search, mobile navigation, theme switching,
code copying, semantic notices, and reduced-motion support when redesigning.
