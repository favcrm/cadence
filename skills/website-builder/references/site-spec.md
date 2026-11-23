# Site spec

What a site built by this playbook must be.

- Plain static files: `index.html` plus CSS, JS, SVG and PNG. No build step, no
  server code, no npm.
- One source folder holds `index.html`. Every local reference is relative or
  root-relative and points at a file in that folder. A reference resolves
  against the page that contains it, so `pages/about.html` may use `../logo.png`.
- `build.py` writes each asset as `<sha256>.<ext>` in its own directory and rewrites
  `href`, `src`, `content` and stylesheet asset `url(...)` references to match.
  `index.html` keeps its name. CSS imports are unsupported; combine stylesheets
  before building.
- Only owner-supplied text, images and facts appear. No invented clients, testimonials,
  prices or results.
- A contact form is optional. If requested, it posts JSON to `/__forms/contact`; the
  host may answer 405, which means no form is available.
- The site is served byte for byte: the live `/` must match `dist/index.html`.
