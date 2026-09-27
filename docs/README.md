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

## Cloudflare Workers

Connect the `cachix/casita` GitHub repository to a Cloudflare Worker with
Workers Builds and these settings:

| Setting | Value |
| --- | --- |
| Production branch | `main` |
| Root directory | `docs` |
| Build command | `npm run build` |
| Deploy command | `npx wrangler deploy` |

Pushes to `main` then publish the site. After the Worker deploys, add
`casita.rs` under **Settings > Domains & Routes > Add > Custom Domain**.

`wrangler.jsonc` configures `dist` as static assets and `worker.js` as the
request handler. The handler serves `/api/github` and responds to Markdown
requests through the site-kit middleware. For a local preview, run
`npx wrangler dev` from `docs` after building the site.

The Worker does not require a `BASIC_AUTH_PASSWORD` secret.
