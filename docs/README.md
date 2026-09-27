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

The site is deployed from this directory by the `casita` Cloudflare Pages
project. Pushes to `main` run `npm run build` and publish `dist/`; pull requests
receive preview deployments. The `/api/stars` endpoint is implemented as a
Pages Function under `functions/`.

`wrangler.jsonc` contains the Pages runtime configuration and can also be used
for local previews with `wrangler pages dev` after building the site.

### Temporary preview password

[`functions/_middleware.js`](functions/_middleware.js) protects the Pages site
with HTTP Basic Auth. The username is `friends`; the password is stored only as
the encrypted `BASIC_AUTH_PASSWORD` Cloudflare Pages secret.

Set or rotate the password from this directory with:

```sh
wrangler pages secret put BASIC_AUTH_PASSWORD --project-name casita
```

When the preview no longer needs protection, delete the middleware, deploy
again, and delete the `BASIC_AUTH_PASSWORD` secret from the Pages project.
