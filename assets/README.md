Edit **slerm.svg** to change the icon. It contains only vector shapes and SVG
gradients, with named groups for the tile, slime, highlights, and terminal prompt.

On macOS, regenerate the checked-in PNG and ICNS with:

```sh
nix run .#make-icon
```

Run this from the repository root. The flake supplies the pinned `resvg`
renderer and uses macOS's built-in `iconutil`; no manual dependencies are needed.
The application uses `slerm.png` at runtime; the macOS bundle uses `slerm.icns`
for Finder and Dock icons. Both are generated from the same SVG.
