# Site spec

What a site built by this playbook must be.

- Plain static files: `index.html` plus CSS, JS, SVG and PNG. No build step, no
  server code, no npm.
- One source folder holds `index.html`. Every local reference is relative or
  root-relative and points at a file in that folder.
- `build.py` writes each asset as `<sha256>.<ext>` in its own directory and rewrites
  `href`, `src` and `content` references to match. `index.html` keeps its name.
- Only owner-supplied text, images and facts appear. No invented clients, testimonials,
  prices or results.
- A contact form is optional. If requested, it posts JSON to `/__forms/contact`; the
  host may answer 405, which means no form is available.
- The site is served byte for byte: the live `/` must match `dist/index.html`.
