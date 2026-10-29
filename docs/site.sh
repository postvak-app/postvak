#!/usr/bin/env bash
# Assemble postvak.pataar.nl: the landing page from postvak-app/website at the root, this book under /docs/.
#
# Usage: docs/site.sh WEBSITE OUT
#   WEBSITE is a checkout of postvak-app/website, OUT the directory to write (emptied first). The pages the book used to
#   serve at the root get a redirect into /docs/, since released binaries and old links still point there.
set -euo pipefail

website=$(realpath "${1:?usage: site.sh WEBSITE OUT}")
out=${2:?usage: site.sh WEBSITE OUT}
book=$(dirname "$(realpath "$0")")

rm -rf "$out"
mkdir -p "$out"
out=$(realpath "$out")

cp -r "$website"/. "$out"/
rm -rf "$out"/.git "$out"/.github "$out"/README.md

mdbook build "$book" --dest-dir "$out/docs"

for page in "$out"/docs/*.html; do
    name=$(basename "$page")
    [ -e "$out/$name" ] && continue
    # The book is built with site-url /docs/, so its 404 page loads its styles from any path.
    [ "$name" = 404.html ] && cp "$page" "$out/404.html" && continue
    target="/docs/$name"
    cat >"$out/$name" <<EOF
<!doctype html>
<meta charset="utf-8">
<meta name="robots" content="noindex">
<title>Moved to $target</title>
<link rel="canonical" href="$target">
<meta http-equiv="refresh" content="0; url=$target">
<script>location.replace("$target" + location.hash)</script>
<a href="$target">$target</a>
EOF
done

{
    echo '<?xml version="1.0" encoding="UTF-8"?>'
    echo '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">'
    echo '<url><loc>https://postvak.pataar.nl/</loc></url>'
    echo '<url><loc>https://postvak.pataar.nl/docs/</loc></url>'
    for page in "$out"/docs/*.html; do
        name=$(basename "$page")
        case $name in 404.html | index.html | print.html | toc.html) continue ;; esac
        echo "<url><loc>https://postvak.pataar.nl/docs/$name</loc></url>"
    done
    echo '</urlset>'
} >"$out/sitemap.xml"
