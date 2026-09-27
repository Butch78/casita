# Casita documentation site

The approved visual identity is documented in [Branding](../BRANDING.md).
Canonical Casita logos are in `public/brand/`; `BrandLogo.astro` selects
the correct artwork for light and dark themes.

The Astro/Starlight site contains task-oriented guides for the generic
`Repository` API.

```console
$ cd docs
$ npm install
$ npm run dev
```

Production builds use `npm run build`.

Rust code blocks in the library, guide, concept, and API pages are included in
Casita's doctests. Check them against the current public API with:

```console
$ devenv shell cargo test -p casita --doc --all-features
```

Lines beginning with `# ` inside Rust blocks provide compile-only setup and
are hidden when the site renders them. The CI test job runs these doctests.

## Cloudflare Pages

Connect the `cachix/casita` GitHub repository to a Cloudflare Pages project with
these build settings:

| Setting | Value |
| --- | --- |
| Production branch | `main` |
| Root directory | `docs` |
| Build command | `npm run build` |
| Build output directory | `dist` |

Pushes to `main` then publish the site, and pull requests receive preview
deployments. Add `casita.rs` as a custom domain on the Pages project. The
`/api/stars` endpoint is implemented as a Pages Function under `functions/`.

`wrangler.jsonc` contains the Pages runtime configuration and can also be used
for local previews with `wrangler pages dev` after building the site.

[`functions/_middleware.js`](functions/_middleware.js) handles Markdown
responses without requiring a password. Remove any old `BASIC_AUTH_PASSWORD`
secret from the Pages project settings.
