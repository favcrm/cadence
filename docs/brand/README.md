# Brand assets

The mark is a terminal prompt: a lit chevron, its dim echo, and a block cursor.

| File | Use |
|---|---|
| `ui/public/icon.svg` | Master, 1024 square, dark tile `#0B0B0C`, corner radius 224. README header, app icon. |
| `ui/public/favicon.svg` | Flat `#2DD4BF` single chevron + cursor for 16 to 32 px. Linked from `ui/index.html`. |
| `ui/public/apple-touch-icon.png` | 180 px raster of the master. |
| `docs/brand/icon-mark.svg` | Transparent mark without the tile, for dark surfaces. |
| `docs/brand/icon-512.png` | 512 px raster of the master. |
| `docs/brand/social-preview.png` | 1280 by 640 for the GitHub repository social preview (Settings, General, Social preview). |
| `ui/src/components/Logo.tsx` | The mark at UI sizes (sidebar). |

Palette: tile `#0B0B0C`, accent `#2DD4BF`, lit gradient `#2DD4BF` to `#22D3EE` at 35 degrees. Regenerate rasters with `rsvg-convert -w <px> ui/public/icon.svg -o <out>.png`.
