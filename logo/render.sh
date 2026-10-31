#!/bin/sh
# Sinkon logo rasteriser.
#
# Renders every SVG in logo/ to the PNG sizes the Android launcher needs.
# Backend order:  resvg  ->  rsvg-convert  ->  node + @resvg/resvg-js
#
#   ./logo/render.sh              # render everything
#   ./logo/render.sh --check      # render nothing, verify existing PNGs
#   RESVG=/path/to/resvg ./logo/render.sh
#
# Outputs are written into logo/png/ and android/app/src/main/res/ so the
# Gradle build picks them up with no extra copy step.
set -eu

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$HERE/.." && pwd)
OUT="$HERE/png"
RES="$ROOT/android/app/src/main/res"
STORE="$HERE/play-store"

MODE=render
[ "${1:-}" = "--check" ] && MODE=check

log() { printf '[sinkon-logo] %s\n' "$*"; }

# ---------------------------------------------------------------- backends
pick_backend() {
    if [ -n "${RESVG:-}" ] && command -v "$RESVG" >/dev/null 2>&1; then
        BACKEND=resvg; BIN=$RESVG; return 0
    fi
    if command -v resvg >/dev/null 2>&1; then
        BACKEND=resvg; BIN=resvg; return 0
    fi
    if command -v rsvg-convert >/dev/null 2>&1; then
        BACKEND=rsvg; BIN=rsvg-convert; return 0
    fi
    if command -v node >/dev/null 2>&1; then
        for cand in "$HERE" "$ROOT" "$PWD"; do
            if [ -d "$cand/node_modules/@resvg/resvg-js" ]; then
                BACKEND=node; BIN=$cand; return 0
            fi
        done
    fi
    cat >&2 <<'MSG'
[sinkon-logo] error: no SVG rasteriser found.
[sinkon-logo] Install one of:
[sinkon-logo]   cargo install resvg            (preferred)
[sinkon-logo]   apt install librsvg2-bin       (rsvg-convert)
[sinkon-logo]   npm --prefix logo i @resvg/resvg-js
MSG
    return 2
}

render_png() { # <svg> <out.png> <width>
    _svg=$1; _out=$2; _w=$3
    mkdir -p "$(dirname -- "$_out")"
    case $BACKEND in
        resvg) "$BIN" --width "$_w" "$_svg" "$_out" >/dev/null ;;
        rsvg)  "$BIN" --width="$_w" --height="$_w" --format=png \
                      --output="$_out" "$_svg" ;;
        node)  node -e '
                 const {Resvg} = require(process.argv[1] + "/node_modules/@resvg/resvg-js");
                 const fs = require("fs");
                 const svg = fs.readFileSync(process.argv[2], "utf8");
                 const width = Number(process.argv[4]);
                 const r = new Resvg(svg, {fitTo: {mode: "width", value: width}});
                 fs.writeFileSync(process.argv[3], r.render().asPng());
               ' "$BIN" "$_svg" "$_out" "$_w" ;;
    esac
    log "rendered $(basename -- "$_out") (${_w}px)"
}

png_width() { # <png>  -- reads the IHDR width with od(1), no dependencies
    od -An -tu1 -j16 -N4 "$1" | awk '{print $1*16777216+$2*65536+$3*256+$4}'
}

# ---------------------------------------------------------------- manifest
# <svg> <png> <width>  rows
manifest() {
    cat <<EOF
$HERE/sinkon.svg $OUT/sinkon-1024.png 1024
$HERE/sinkon.svg $OUT/sinkon-512.png 512
$HERE/sinkon.svg $OUT/sinkon-192.png 192
$HERE/sinkon.svg $OUT/sinkon-48.png 48
$HERE/sinkon.svg $OUT/sinkon-24.png 24
$HERE/sinkon.svg $STORE/icon-512.png 512
$HERE/sinkon-wordmark.svg $OUT/sinkon-wordmark.png 1480
$HERE/sinkon-monochrome.svg $OUT/sinkon-monochrome-256.png 256
$HERE/sinkon-monochrome-inverse.svg $OUT/sinkon-monochrome-inverse-256.png 256
$HERE/sinkon-badge-vulkan.svg $OUT/sinkon-badge-vulkan-128.png 128
$HERE/sinkon-badge-gl.svg $OUT/sinkon-badge-gl-128.png 128
$HERE/sinkon-badge-auto.svg $OUT/sinkon-badge-auto-128.png 128
$HERE/sinkon.svg $RES/drawable-nodpi/sinkon_renderer_badge.png 256
$HERE/sinkon-badge-vulkan.svg $RES/drawable-nodpi/sinkon_badge_vulkan.png 128
$HERE/sinkon-badge-gl.svg $RES/drawable-nodpi/sinkon_badge_gl.png 128
$HERE/sinkon-badge-auto.svg $RES/drawable-nodpi/sinkon_badge_auto.png 128
$HERE/sinkon.svg $RES/mipmap-mdpi/ic_launcher.png 48
$HERE/sinkon.svg $RES/mipmap-hdpi/ic_launcher.png 72
$HERE/sinkon.svg $RES/mipmap-xhdpi/ic_launcher.png 96
$HERE/sinkon.svg $RES/mipmap-xxhdpi/ic_launcher.png 144
$HERE/sinkon.svg $RES/mipmap-xxxhdpi/ic_launcher.png 192
$HERE/sinkon-icon-foreground.svg $RES/mipmap-mdpi/ic_launcher_foreground.png 108
$HERE/sinkon-icon-foreground.svg $RES/mipmap-hdpi/ic_launcher_foreground.png 162
$HERE/sinkon-icon-foreground.svg $RES/mipmap-xhdpi/ic_launcher_foreground.png 216
$HERE/sinkon-icon-foreground.svg $RES/mipmap-xxhdpi/ic_launcher_foreground.png 324
$HERE/sinkon-icon-foreground.svg $RES/mipmap-xxxhdpi/ic_launcher_foreground.png 432
$HERE/sinkon-icon-background.svg $RES/mipmap-mdpi/ic_launcher_background.png 108
$HERE/sinkon-icon-background.svg $RES/mipmap-hdpi/ic_launcher_background.png 162
$HERE/sinkon-icon-background.svg $RES/mipmap-xhdpi/ic_launcher_background.png 216
$HERE/sinkon-icon-background.svg $RES/mipmap-xxhdpi/ic_launcher_background.png 324
$HERE/sinkon-icon-background.svg $RES/mipmap-xxxhdpi/ic_launcher_background.png 432
EOF
}

# ---------------------------------------------------------------- validate
validate_svgs() {
    for svg in "$HERE"/*.svg; do
        if command -v xmllint >/dev/null 2>&1; then
            xmllint --noout "$svg" || { log "FAIL: $svg is not well-formed XML"; exit 1; }
        fi
        for bad in '<image' '<foreignObject' 'base64,' 'xlink:href="data:'; do
            if grep -qF -- "$bad" "$svg"; then
                log "FAIL: $svg contains forbidden construct: $bad"; exit 1
            fi
        done
        log "validated $(basename -- "$svg")"
    done
}

# ---------------------------------------------------------------- modes
if [ "$MODE" = check ]; then
    fail=0
    while read -r _svg _png _w; do
        if [ ! -f "$_png" ]; then log "MISSING $_png"; fail=1; continue; fi
        got=$(png_width "$_png")
        if [ "$got" != "$_w" ]; then
            log "WRONG-SIZE $_png is ${got}px, expected ${_w}px"; fail=1
        fi
    done <<EOF
$(manifest)
EOF
    [ "$fail" -eq 0 ] && log "all PNGs present at the expected sizes"
    exit "$fail"
fi

pick_backend || exit $?
log "rasteriser: $BACKEND"
validate_svgs

while read -r _svg _png _w; do
    render_png "$_svg" "$_png" "$_w"
done <<EOF
$(manifest)
EOF

log "done -- $(manifest | wc -l | tr -d ' ') PNGs"
